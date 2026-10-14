//! Decode and handshake work must not queue behind an application handler.
//! WebSocket control frames and the read deadline live in `socket_io`; this
//! task also routes accepted application heartbeats around game computation.

use axum::extract::ws::Message;
use lobby_broker::LobbyRegistration;
use server_core::client_message_wire_guard::{
    guard_client_message_before_dispatch, wire_rejection_message,
};
use server_core::protocol::{ClientMessage, ServerMessage, ServerMode};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, info, warn};

use crate::socket_io::{NegotiatedSocket, Received};
use crate::{
    classify_hello_gate, hello_acceptance, reject_if_disabled, wire, ClientHelloInfo,
    HelloGateOutcome, SharedLobby, SysEnv, MAX_WS_MESSAGE_BYTES,
};

pub(crate) async fn run(
    mut frames: mpsc::UnboundedReceiver<Received<Message>>,
    mut socket: NegotiatedSocket,
    lobby: SharedLobby,
    host: watch::Receiver<Option<LobbyRegistration>>,
    mode: ServerMode,
    hello: oneshot::Sender<ClientHelloInfo>,
    application: mpsc::UnboundedSender<Received<ClientMessage>>,
) {
    let mut hello = Some(hello);
    while let Some(Received {
        message: frame,
        received_at,
    }) = frames.recv().await
    {
        let text = match frame {
            Message::Text(text) => text,
            Message::Binary(bytes) if socket.uses_gzip_envelope() => {
                match wire::decode_client_envelope(bytes.to_vec(), MAX_WS_MESSAGE_BYTES).await {
                    Ok(text) => text.into(),
                    Err(error) => {
                        warn!(%error, "failed to decode client binary envelope");
                        continue;
                    }
                }
            }
            Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Close(_) => {
                continue
            }
        };
        let message: ClientMessage = match serde_json::from_str(&text) {
            Ok(message) => message,
            Err(error) => {
                warn!(%error, "failed to parse client message");
                let reply = ServerMessage::error(format!("Invalid message: {error}"));
                if send_reply(&mut socket, &reply).await.is_err() {
                    break;
                }
                continue;
            }
        };
        let rejection = match classify_hello_gate(hello.is_none(), &message, hello_acceptance(mode))
        {
            HelloGateOutcome::Accept(info) => {
                info!(version = %info.client_version, commit = %info.build_commit, "ClientHello accepted");
                socket.set_wire_format(info.wire_format);
                if hello
                    .take()
                    .expect("first accepted hello owns the handoff")
                    .send(info)
                    .is_err()
                {
                    break;
                }
                continue;
            }
            HelloGateOutcome::RejectInvalidHello(reason) => {
                warn!(%reason, "ClientHello rejected at wire guard");
                Some(ServerMessage::error(reason))
            }
            HelloGateOutcome::RejectProtocol { client, server } => {
                warn!(
                    client_protocol = client,
                    server_protocol = server,
                    "protocol version mismatch at ClientHello"
                );
                let remedy = if client < server {
                    "Please update your client."
                } else {
                    "This server is older than your client; wait for the rollout to complete."
                };
                Some(ServerMessage::error(format!(
                    "Protocol version mismatch (client={client} server={server}). {remedy}"
                )))
            }
            HelloGateOutcome::RejectHandshakeRequired => {
                warn!("client sent non-hello message before ClientHello");
                Some(ServerMessage::error(
                    "ClientHello required before any other message".to_string(),
                ))
            }
            HelloGateOutcome::IgnoreRedundantHello => {
                debug!("ignoring redundant ClientHello");
                continue;
            }
            HelloGateOutcome::PassThrough => None,
        };
        if let Some(rejection) = rejection {
            if send_reply(&mut socket, &rejection).await.is_err() {
                break;
            }
            continue;
        }
        if let ClientMessage::Ping { timestamp } = &message {
            if let Some(reason) = reject_if_disabled(&message, mode) {
                if send_reply(&mut socket, &ServerMessage::error(reason.to_string()))
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
            if let Err(reason) = guard_client_message_before_dispatch(&message, mode) {
                if send_reply(&mut socket, &wire_rejection_message(&message, reason))
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
            // Clone the stamp, not a watch read guard, across the broker await.
            let registration = host.borrow().clone();
            if let Some(registration) = registration {
                let mut broker = lobby.lock().await;
                broker.lobby_mut().refresh_liveness(&registration, &SysEnv);
            }
            // The broker guard is gone before any network backpressure.
            if socket.enqueue_pong(*timestamp).await.is_err() {
                break;
            }
        } else if application
            .send(Received {
                message,
                received_at,
            })
            .is_err()
        {
            break;
        }
    }
}

async fn send_reply(socket: &NegotiatedSocket, reply: &ServerMessage) -> Result<(), String> {
    socket.enqueue_reply(reply).await
}
