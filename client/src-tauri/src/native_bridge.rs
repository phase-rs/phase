use futures_util::{
    future::{AbortHandle, AbortRegistration, Abortable},
    SinkExt, StreamExt,
};
use serde::Serialize;
use tauri::ipc::Channel;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{header::ORIGIN, HeaderValue, Request},
        protocol::Message,
    },
    MaybeTlsStream, WebSocketStream,
};

use crate::native_engine;

/// Events forwarded from the shell-owned loopback WebSocket to remote content.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BridgeEvent {
    Message { text: String },
    Closed { code: u16, reason: String },
    Error { detail: String },
}

/// Structured failures from the pinned bridge commands.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeEngineBridgeError {
    NotRunning { detail: String },
    Connect { detail: String },
    UnknownBridge { detail: String },
    Send { detail: String },
    Internal { detail: String },
}

impl NativeEngineBridgeError {
    fn internal(detail: impl Into<String>) -> Self {
        Self::Internal {
            detail: detail.into(),
        }
    }
}

pub(crate) struct BridgeHandle {
    abort: AbortHandle,
    outbound: UnboundedSender<Message>,
}

impl BridgeHandle {
    pub(crate) fn new(abort: AbortHandle, outbound: UnboundedSender<Message>) -> Self {
        Self { abort, outbound }
    }

    pub(crate) fn abort(&self) {
        self.abort.abort();
    }

    pub(crate) fn outbound(&self) -> UnboundedSender<Message> {
        self.outbound.clone()
    }
}

/// Opens the shell-pinned loopback connection for the currently running engine.
#[tauri::command]
pub async fn connect_native_engine(
    on_event: Channel<BridgeEvent>,
) -> Result<u64, NativeEngineBridgeError> {
    let (outbound, receiver) = mpsc::unbounded_channel();
    let (abort, registration) = AbortHandle::new_pair();
    let (bridge_id, port, origin) = native_engine::register_native_engine_bridge(
        BridgeHandle::new(abort, outbound),
    )
    .map_err(|error| match error {
        native_engine::NativeBridgeRegistryError::NotRunning => {
            NativeEngineBridgeError::NotRunning {
                detail: "no native engine is running".to_owned(),
            }
        }
        native_engine::NativeBridgeRegistryError::Internal(detail) => {
            NativeEngineBridgeError::internal(detail)
        }
    })?;

    let request = match bridge_request(port, origin) {
        Ok(request) => request,
        Err(error) => {
            native_engine::close_native_engine_bridge(bridge_id);
            return Err(error);
        }
    };
    let socket = match connect_async(request).await {
        Ok((socket, _)) => socket,
        Err(error) => {
            native_engine::close_native_engine_bridge(bridge_id);
            return Err(NativeEngineBridgeError::Connect {
                detail: error.to_string(),
            });
        }
    };

    tauri::async_runtime::spawn(async move {
        // The command response is queued before this task begins forwarding frames.
        // NativeEngineSocket also queues Channel callbacks until its invoke resolves,
        // which preserves the WebSocket open-before-message contract at the JS boundary.
        tokio::task::yield_now().await;
        forward_bridge(bridge_id, socket, receiver, registration, on_event).await;
    });

    Ok(bridge_id)
}

/// Sends a JSON text frame over a shell-pinned bridge.
#[tauri::command]
pub fn native_engine_bridge_send(id: u64, text: String) -> Result<(), NativeEngineBridgeError> {
    let outbound = native_engine::native_engine_bridge_sender(id).ok_or_else(|| {
        NativeEngineBridgeError::UnknownBridge {
            detail: format!("native engine bridge {id} is not open"),
        }
    })?;
    outbound
        .send(Message::Text(text.into()))
        .map_err(|error| NativeEngineBridgeError::Send {
            detail: error.to_string(),
        })
}

/// Closes a shell-pinned bridge and forwards a close event to JS.
#[tauri::command]
pub fn native_engine_bridge_close(id: u64) -> Result<(), NativeEngineBridgeError> {
    if native_engine::close_native_engine_bridge(id) {
        Ok(())
    } else {
        Err(NativeEngineBridgeError::UnknownBridge {
            detail: format!("native engine bridge {id} is not open"),
        })
    }
}

fn bridge_request(port: u16, origin: &str) -> Result<Request<()>, NativeEngineBridgeError> {
    let url = format!("ws://127.0.0.1:{port}");
    let mut request =
        url.into_client_request()
            .map_err(|error| NativeEngineBridgeError::Connect {
                detail: error.to_string(),
            })?;
    let origin =
        HeaderValue::from_str(origin).map_err(|error| NativeEngineBridgeError::Connect {
            detail: error.to_string(),
        })?;
    request.headers_mut().insert(ORIGIN, origin);
    Ok(request)
}

async fn forward_bridge(
    bridge_id: u64,
    socket: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    outbound: UnboundedReceiver<Message>,
    registration: AbortRegistration,
    on_event: Channel<BridgeEvent>,
) {
    let result = Abortable::new(run_bridge(socket, outbound, on_event.clone()), registration).await;
    let (error, close) = match result {
        Ok((error, close)) => (error, close),
        Err(_) => (None, None),
    };

    if let Some(detail) = error {
        let _ = on_event.send(BridgeEvent::Error { detail });
    }
    let (code, reason) = close.unwrap_or((1006, String::new()));
    let _ = on_event.send(BridgeEvent::Closed { code, reason });
    native_engine::remove_native_engine_bridge(bridge_id);
}

async fn run_bridge(
    socket: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    mut outbound: UnboundedReceiver<Message>,
    on_event: Channel<BridgeEvent>,
) -> (Option<String>, Option<(u16, String)>) {
    let (mut write, mut read) = socket.split();
    let mut error = None;
    let mut close = None;

    loop {
        tokio::select! {
            outgoing = outbound.recv() => match outgoing {
                Some(message) => {
                    if let Err(send_error) = write.send(message).await {
                        error = Some(send_error.to_string());
                        break;
                    }
                }
                None => break,
            },
            incoming = read.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if on_event.send(BridgeEvent::Message { text: text.to_string() }).is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Binary(_))) => {
                    error = Some("native engine bridge received an unsupported binary frame".to_owned());
                    break;
                }
                Some(Ok(Message::Ping(_))) => {
                    if let Err(flush_error) = write.flush().await {
                        error = Some(flush_error.to_string());
                        break;
                    }
                }
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(frame))) => {
                    close = frame.map(|frame| (u16::from(frame.code), frame.reason.to_string()));
                    if let Err(flush_error) = write.flush().await {
                        error = Some(flush_error.to_string());
                    }
                    break;
                }
                Some(Ok(Message::Frame(_))) => {}
                Some(Err(read_error)) => {
                    error = Some(read_error.to_string());
                    break;
                }
                None => break,
            },
        }
    }

    (error, close)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_events_use_camel_case_discriminants() {
        assert_eq!(
            serde_json::to_string(&BridgeEvent::Message {
                text: "frame".to_owned(),
            })
            .unwrap(),
            r#"{"type":"message","text":"frame"}"#
        );
        assert_eq!(
            serde_json::to_string(&BridgeEvent::Closed {
                code: 1000,
                reason: "normal".to_owned(),
            })
            .unwrap(),
            r#"{"type":"closed","code":1000,"reason":"normal"}"#
        );
        assert_eq!(
            serde_json::to_string(&BridgeEvent::Error {
                detail: "read failed".to_owned(),
            })
            .unwrap(),
            r#"{"type":"error","detail":"read failed"}"#
        );
    }

    #[test]
    fn bridge_request_is_loopback_and_uses_the_channel_origin() {
        let request = bridge_request(43123, "https://phase-rs.dev").unwrap();

        assert_eq!(request.uri().to_string(), "ws://127.0.0.1:43123/");
        assert_eq!(
            request.headers()[ORIGIN].to_str().unwrap(),
            "https://phase-rs.dev"
        );
    }

    #[test]
    fn connect_without_a_running_engine_returns_not_running() {
        let channel = Channel::new(|_| Ok(()));
        let result = tauri::async_runtime::block_on(connect_native_engine(channel));

        assert!(matches!(
            result,
            Err(NativeEngineBridgeError::NotRunning { detail })
                if detail == "no native engine is running"
        ));
    }

    #[test]
    fn send_and_close_unknown_bridge_return_unknown_bridge() {
        let unknown_bridge_id = u64::MAX;

        assert!(matches!(
            native_engine_bridge_send(unknown_bridge_id, "frame".to_owned()),
            Err(NativeEngineBridgeError::UnknownBridge { .. })
        ));
        assert!(matches!(
            native_engine_bridge_close(unknown_bridge_id),
            Err(NativeEngineBridgeError::UnknownBridge { .. })
        ));
    }
}
