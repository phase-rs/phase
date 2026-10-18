//! Decode and handshake work must not queue behind an application handler.
//! WebSocket control frames and the read deadline live in `socket_io`; this
//! task also routes accepted application heartbeats around game computation.
//! Native Full and LobbyOnly sockets answer Pong here without game admission.
//! Shared/WASM broker callers retain `LobbyClientMessage::Ping` parity; hosted
//! listing refreshes still use the broker's registration-aware liveness kernel.

use axum::extract::ws::Message;
use lobby_broker::LobbyRegistration;
use server_core::client_message_wire_guard::{
    guard_client_message_before_dispatch, wire_rejection_message,
};
use server_core::protocol::{ClientMessage, ServerMessage, ServerMode};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, info, warn};

use crate::socket_io::{HeartbeatInspection, NegotiatedSocket, Received};
use crate::{
    classify_hello_gate, hello_acceptance, reject_if_disabled, wire, ClientHelloInfo,
    HelloGateOutcome, SharedLobby, SysEnv, MAX_WS_MESSAGE_BYTES,
};

enum DecoderWake {
    Heartbeat,
    Frame(Option<Received<Message>>),
}

enum RateCharge {
    Applied,
    RefusalInspection,
}

// Per frozen window: <=30 ordinary parses, <=30 nonrefunded recovery scans,
// <=30 compact-click scans transferred to their own allowance, and <=30 typed
// compact clicks. Add <=64 charged reader probes: <=184 JSON entries total.
// Binary envelope decode/inflation has <=90 admissions in that window
// (30 ordinary +30 retained recovery +30 transferred clicks), capped at 64 KiB.
// A click refund is allowed only after its separate bounded admission succeeds,
// so it cannot replenish unlimited scan work or drain correlated refusals.
// Flush-held admission bounds only correlated quota refusals, not guard replies.
const MAX_RECOVERY_CLICK_BYTES: usize = 1024;
pub(crate) const MAX_QUOTA_DISCARDS_PER_WINDOW: u32 = 64;

pub(crate) async fn run(
    mut frames: mpsc::Receiver<Received<Message>>,
    mut socket: NegotiatedSocket,
    lobby: SharedLobby,
    host: watch::Receiver<Option<LobbyRegistration>>,
    mode: ServerMode,
    hello: oneshot::Sender<ClientHelloInfo>,
    application: mpsc::Sender<Received<crate::socket_mailbox::ClientEnvelope>>,
) {
    let mut hello = Some(hello);
    let mut limiter = IngressRateLimiter::new();
    let mut refusal_inspection = crate::RateLimiter::new();
    let mut recovered_clicks = crate::RateLimiter::new();
    let mut discards = crate::RateLimiter::with_limit(MAX_QUOTA_DISCARDS_PER_WINDOW);
    #[cfg(test)]
    let parser_work = socket.bootstrap_hooks().take_parser_work();
    #[cfg(test)]
    if let Some(work) = &parser_work {
        limiter.inner.freeze(work.quota_at());
        socket.observe_probe_work(work.clone());
        refusal_inspection.freeze(work.quota_at());
        recovered_clicks.freeze(work.quota_at());
        discards.freeze(work.quota_at());
    }
    let mut refresh: Option<HostRefresh> = None;
    loop {
        let wake = tokio::select! {
            _ = socket.application_activity() => DecoderWake::Heartbeat,
            frame = frames.recv() => DecoderWake::Frame(frame),
        };
        let Received {
            message: frame,
            received_at,
            mut admission,
            heartbeat,
        } = match wake {
            DecoderWake::Heartbeat => {
                if host.borrow().is_some() {
                    refresh
                        .get_or_insert_with(|| HostRefresh::start(lobby.clone(), host.clone()))
                        .notify();
                }
                continue;
            }
            DecoderWake::Frame(Some(frame)) => frame,
            DecoderWake::Frame(None) => break,
        };
        #[cfg(test)]
        let _completed = parser_work.as_ref().map(|work| work.frame_guard());
        // Frames read before ClientHello acceptance were not reader-probed.
        // Use the SAME charged allowance once negotiation finishes, never twice.
        if hello.is_none() && matches!(heartbeat, HeartbeatInspection::Deferred) {
            if let Some(timestamp) = socket.probe_heartbeat(&frame) {
                socket.enqueue_pong(timestamp);
                continue;
            }
        }
        // Admit before deserialization, metadata recovery or inflation.
        // Every reader parse spends probe-work credit (64 burst + 64/s refill).
        // <=30 full parses + <=60 metadata scans + <=30 typed compact clicks.
        // Refunded click scans have their own 30-entry ceiling. Discarding does
        // no parsing/inflation; >64 discards in one window closes sustained abuse
        // without turning a legitimate contest burst into a transport drop.
        if !matches!(&frame, Message::Text(_) | Message::Binary(_)) {
            continue;
        }
        let charge = if limiter.allow() {
            RateCharge::Applied
        } else {
            #[cfg(test)]
            if let Some(work) = &parser_work {
                work.rate_limited();
            }
            // CommandIdV1 submissions are sequential. Even a large action just
            // after a legitimate click burst gets an owned refusal: recovery
            // skips payload structures and never dispatches an engine command.
            // Only traffic past BOTH work windows is discarded unparsed.
            if refusal_inspection.check() {
                RateCharge::RefusalInspection
            } else {
                if !discard_quota_frame(&socket, &mut discards) {
                    break;
                }
                continue;
            }
        };
        let text = match frame {
            Message::Text(text) => text,
            Message::Binary(bytes) if socket.uses_gzip_envelope() => {
                match wire::decode_client_envelope(bytes.to_vec(), MAX_WS_MESSAGE_BYTES).await {
                    Ok(text) => text.into(),
                    Err(error) => {
                        limiter.warn_malformed_envelope(&error);
                        continue;
                    }
                }
            }
            Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Close(_) => {
                continue
            }
        };
        if let Err(limit) = admission.resize(text.len()) {
            warn!(
                ?limit,
                "decoded WebSocket ingress payload capacity exceeded"
            );
            socket.ingress_overflow();
            break;
        }
        if matches!(charge, RateCharge::RefusalInspection) {
            #[cfg(test)]
            if let Some(work) = &parser_work {
                work.metadata_recovery();
            }
            match crate::socket_mailbox::ClientEnvelope::quota_recovery(&text) {
                Some(crate::socket_mailbox::QuotaRecovery::ContestClick)
                    if text.len() <= MAX_RECOVERY_CLICK_BYTES && recovered_clicks.check() =>
                {
                    refusal_inspection.refund();
                }
                Some(crate::socket_mailbox::QuotaRecovery::Correlated(Some(id))) => {
                    if socket.enqueue_quota_refusal(Some(id)).is_err() {
                        break;
                    }
                    continue;
                }
                Some(
                    crate::socket_mailbox::QuotaRecovery::Correlated(None)
                    | crate::socket_mailbox::QuotaRecovery::ContestClick
                    | crate::socket_mailbox::QuotaRecovery::Other,
                )
                | None => {
                    if !discard_quota_frame(&socket, &mut discards) {
                        break;
                    }
                    continue;
                }
            }
        }
        #[cfg(test)]
        if let Some(work) = &parser_work {
            work.deserialization();
        }
        let envelope: crate::socket_mailbox::ClientEnvelope = match serde_json::from_str(&text) {
            Ok(message) => message,
            Err(error) => {
                if matches!(charge, RateCharge::RefusalInspection) {
                    if !discard_quota_frame(&socket, &mut discards) {
                        break;
                    }
                    continue;
                }
                let command_id = if refusal_inspection.check() {
                    #[cfg(test)]
                    if let Some(work) = &parser_work {
                        work.metadata_recovery();
                    }
                    crate::socket_mailbox::ClientEnvelope::malformed_command_id(&text)
                } else {
                    None
                };
                warn!(%error, "failed to parse client message");
                let message = format!("Invalid message: {error}");
                let rejection = if command_id.is_some() {
                    ServerMessage::ActionFailed { message }
                } else {
                    ServerMessage::error(message)
                };
                if socket.enqueue_completion(rejection, command_id).is_err() {
                    break;
                }
                continue;
            }
        };
        let command_id = envelope.command_id();
        let message = &envelope.message;
        let rejection = match classify_hello_gate(hello.is_none(), message, hello_acceptance(mode))
        {
            HelloGateOutcome::Accept(info) => {
                #[cfg(test)]
                socket.bootstrap_hooks().before_hello_accept().await;
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
            let rejection = if command_id.is_some() {
                crate::operation_failed_message(message, "Client handshake rejected this command")
                    .unwrap_or(rejection)
            } else {
                rejection
            };
            if socket.enqueue_completion(rejection, command_id).is_err() {
                break;
            }
            continue;
        }
        // Apply the existing cheap shape guards before a decoded command can
        // wait behind a long handler. Authorization remains in the handler.
        let rejection = if let Some(reason) = reject_if_disabled(message, mode) {
            Some(
                crate::operation_failed_message(message, reason)
                    .unwrap_or_else(|| ServerMessage::error(reason)),
            )
        } else if let Err(reason) = guard_client_message_before_dispatch(message, mode) {
            Some(wire_rejection_message(message, reason))
        } else {
            None
        };
        if let Some(rejection) = rejection {
            if socket.enqueue_completion(rejection, command_id).is_err() {
                break;
            }
            continue;
        }
        if let ClientMessage::Ping { timestamp } = message {
            socket.enqueue_pong(*timestamp);
            if host.borrow().is_some() {
                refresh
                    .get_or_insert_with(|| HostRefresh::start(lobby.clone(), host.clone()))
                    .notify();
            }
        } else {
            match application.try_send(Received {
                message: envelope,
                received_at,
                admission,
                heartbeat: HeartbeatInspection::Complete,
            }) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    socket.ingress_overflow();
                    break;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => break,
            }
        }
    }
}

/// Count every quota discard, including the frame that exhausts the abuse
/// allowance. Closing uses the same bounded drain as ingress/refusal overflow.
fn discard_quota_frame(socket: &NegotiatedSocket, discards: &mut crate::RateLimiter) -> bool {
    socket.record_quota_discard();
    if discards.check() {
        true
    } else {
        warn!(
            allowed_discards_per_window = MAX_QUOTA_DISCARDS_PER_WINDOW,
            "WebSocket quota discard budget exceeded"
        );
        socket.quota_discard_overflow();
        false
    }
}

struct IngressRateLimiter {
    inner: crate::RateLimiter,
    dropped: u64,
    last_warning: Option<std::time::Instant>,
    malformed: u64,
    last_malformed_warning: Option<std::time::Instant>,
}

impl IngressRateLimiter {
    fn new() -> Self {
        Self {
            inner: crate::RateLimiter::new(),
            dropped: 0,
            last_warning: None,
            malformed: 0,
            last_malformed_warning: None,
        }
    }

    fn allow(&mut self) -> bool {
        if self.inner.check() {
            return true;
        }
        self.dropped += 1;
        let now = std::time::Instant::now();
        if self
            .last_warning
            .is_none_or(|last| now.duration_since(last).as_secs() >= crate::RATE_LIMIT_WINDOW_SECS)
        {
            warn!(
                dropped_application_frames = self.dropped,
                "WebSocket application rate limit exceeded"
            );
            self.last_warning = Some(now);
        }
        false
    }

    fn warn_malformed_envelope(&mut self, error: &str) {
        self.malformed += 1;
        let now = std::time::Instant::now();
        if self
            .last_malformed_warning
            .is_none_or(|last| now.duration_since(last).as_secs() >= crate::RATE_LIMIT_WINDOW_SECS)
        {
            warn!(
                %error,
                malformed_binary_frames = self.malformed,
                "failed to decode client binary envelope"
            );
            self.last_malformed_warning = Some(now);
        }
    }
}

/// Hosted listing refreshes coalesce independently of heartbeat replies.
/// One task per hosting connection prevents a blocked broker from accumulating
/// a detached task for every Ping or delaying the decoder's next Pong.
/// This stamp belongs to broker-hosted (LobbyOnly) listings. Native Full rooms
/// and draft pods are kept alive by the session-aware lobby expiry sweep.
struct HostRefresh {
    changed: std::sync::Arc<tokio::sync::Notify>,
    worker: tokio::task::JoinHandle<()>,
}

impl HostRefresh {
    fn start(lobby: SharedLobby, host: watch::Receiver<Option<LobbyRegistration>>) -> Self {
        let changed = std::sync::Arc::new(tokio::sync::Notify::new());
        let worker = tokio::spawn({
            let changed = changed.clone();
            async move {
                loop {
                    changed.notified().await;
                    let registration = host.borrow().clone();
                    if let Some(registration) = registration {
                        let mut broker = lobby.lock().await;
                        broker.lobby_mut().refresh_liveness(&registration, &SysEnv);
                    }
                }
            }
        });
        Self { changed, worker }
    }

    fn notify(&self) {
        self.changed.notify_one();
    }
}

impl Drop for HostRefresh {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
