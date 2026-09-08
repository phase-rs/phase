use std::{
    collections::BTreeMap,
    net::Ipv4Addr,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use futures_util::{
    future::{AbortHandle, AbortRegistration, Abortable},
    SinkExt, StreamExt,
};
use tauri::{ipc::Channel, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
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
use crate::native_engine_contract::{BridgeEvent, NativeEngineBridgeError};

/// phase-server's WebSocket route. Kept as a named constant so the one place
/// that dials it reads as a contract rather than an incidental URL suffix.
const SERVER_WEBSOCKET_PATH: &str = "/ws";

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
        forward_bridge(
            bridge_id,
            socket,
            receiver,
            registration,
            on_event,
            native_engine::remove_native_engine_bridge,
        )
        .await;
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
    // phase-server serves its socket at `/ws` (`crates/phase-server/src/main.rs`
    // route table), the same path web clients dial. The root path answers 404,
    // which fails the upgrade and sends every native session to WASM.
    let url = format!("ws://127.0.0.1:{port}{SERVER_WEBSOCKET_PATH}");
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
    remove: fn(u64),
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
    remove(bridge_id);
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

#[derive(Default)]
struct LanBridges {
    next_id: u64,
    bridges: BTreeMap<u64, LanBridge>,
    consent: LanConsent,
}

#[derive(Default)]
struct LanConsent {
    generation: u64,
    targets: BTreeMap<LanTarget, ConsentDecision>,
}

#[derive(PartialEq)]
enum ConsentDecision {
    Pending,
    Allowed,
    Denied,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct LanClient {
    window: String,
    origin: String,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum LanOperation {
    Connect(String),
    Discover,
    StartHosting,
    StopHosting,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct LanTarget {
    client: LanClient,
    operation: LanOperation,
}

struct LanBridge {
    client: LanClient,
    handle: BridgeHandle,
}

pub(crate) struct LanAuthorization {
    target: LanTarget,
    generation: u64,
}

impl LanAuthorization {
    pub(crate) fn require_current(&self) -> Result<(), NativeEngineBridgeError> {
        let state = lan_bridges()
            .lock()
            .map_err(|error| NativeEngineBridgeError::internal(error.to_string()))?;
        if state.consent.generation != self.generation {
            return Err(consent_error());
        }
        state.consent.require(&self.target)
    }
}

fn consent_error() -> NativeEngineBridgeError {
    NativeEngineBridgeError::Connect {
        detail: "LAN operation requires native approval for this page session".into(),
    }
}

impl LanConsent {
    fn begin(&mut self, target: &LanTarget) -> Result<Option<u64>, NativeEngineBridgeError> {
        match self.targets.get(target) {
            Some(ConsentDecision::Allowed) => Ok(None),
            Some(ConsentDecision::Pending | ConsentDecision::Denied) => Err(consent_error()),
            None => {
                self.targets
                    .insert(target.clone(), ConsentDecision::Pending);
                Ok(Some(self.generation))
            }
        }
    }

    fn finish(
        &mut self,
        target: LanTarget,
        generation: u64,
        allowed: bool,
    ) -> Result<(), NativeEngineBridgeError> {
        if generation != self.generation {
            return Err(consent_error());
        }
        self.targets.insert(
            target,
            if allowed {
                ConsentDecision::Allowed
            } else {
                ConsentDecision::Denied
            },
        );
        if allowed {
            Ok(())
        } else {
            Err(consent_error())
        }
    }

    fn require(&self, target: &LanTarget) -> Result<(), NativeEngineBridgeError> {
        if self.targets.get(target) == Some(&ConsentDecision::Allowed) {
            Ok(())
        } else {
            Err(consent_error())
        }
    }

    fn clear(&mut self) {
        self.generation += 1;
        self.targets.clear();
    }
}

static LAN_BRIDGES: OnceLock<Mutex<LanBridges>> = OnceLock::new();

fn lan_bridges() -> &'static Mutex<LanBridges> {
    LAN_BRIDGES.get_or_init(|| Mutex::new(LanBridges::default()))
}

// Parse the authority as an IPv4 literal before constructing a URL. This
// intentionally excludes DNS, URL-parser legacy numeric hosts, credentials,
// alternate paths and every ambiguous normalization accepted by browsers.
fn lan_endpoint(url: &str) -> Result<String, NativeEngineBridgeError> {
    let invalid = || NativeEngineBridgeError::Connect {
        detail: "expected ws://private-or-loopback-IPv4:port/ws".into(),
    };
    let authority = url
        .strip_prefix("ws://")
        .and_then(|rest| rest.strip_suffix("/ws"))
        .ok_or_else(invalid)?;
    let (host, port) = authority.split_once(':').ok_or_else(invalid)?;
    let ip: Ipv4Addr = host.parse().map_err(|_| invalid())?;
    let port: u16 = port.parse().map_err(|_| invalid())?;
    if !(ip.is_private() || ip.is_loopback()) || port == 0 {
        return Err(invalid());
    }
    Ok(format!("ws://{ip}:{port}/ws"))
}

fn lan_origin(origin: &str) -> Result<&'static str, NativeEngineBridgeError> {
    Ok(match origin {
        "https://phase-rs.dev" | "https://app.phase-rs.dev" => "https://phase-rs.dev",
        "https://preview.phase-rs.dev" => "https://preview.phase-rs.dev",
        _ => {
            return Err(NativeEngineBridgeError::Connect {
                detail: "unsupported LAN client origin".into(),
            })
        }
    })
}

fn lan_request(url: &str, origin: &str) -> Result<Request<()>, NativeEngineBridgeError> {
    let origin = lan_origin(origin)?;
    let mut request = lan_endpoint(url)?.into_client_request().map_err(|error| {
        NativeEngineBridgeError::Connect {
            detail: error.to_string(),
        }
    })?;
    request
        .headers_mut()
        .insert(ORIGIN, HeaderValue::from_static(origin));
    Ok(request)
}

fn invoking_lan_client(window: &WebviewWindow) -> Result<LanClient, NativeEngineBridgeError> {
    let origin = window
        .url()
        .map_err(|error| NativeEngineBridgeError::internal(error.to_string()))?
        .origin()
        .ascii_serialization();
    lan_origin(&origin)?;
    Ok(LanClient {
        window: window.label().to_owned(),
        origin,
    })
}

pub(crate) async fn authorize_lan_operation(
    window: &WebviewWindow,
    operation: LanOperation,
) -> Result<LanAuthorization, NativeEngineBridgeError> {
    let target = LanTarget {
        client: invoking_lan_client(window)?,
        operation,
    };
    let (pending, generation) = {
        let mut state = lan_bridges()
            .lock()
            .map_err(|error| NativeEngineBridgeError::internal(error.to_string()))?;
        (state.consent.begin(&target)?, state.consent.generation)
    };
    if pending.is_some() {
        let action = match &target.operation {
            LanOperation::Connect(endpoint) => {
                format!("connect to {endpoint} on your local network")
            }
            LanOperation::Discover => "search your local network for phase.rs servers".into(),
            LanOperation::StartHosting => {
                "start and advertise a game server accessible on your local network".into()
            }
            LanOperation::StopHosting => {
                "stop your local game server and disconnect its players".into()
            }
        };
        let network_notice = match &target.operation {
            LanOperation::Connect(_) | LanOperation::StartHosting => " LAN traffic is unencrypted: others on the network can read game passwords and reconnect tokens. Use only a trusted LAN.",
            LanOperation::Discover | LanOperation::StopHosting => "",
        };
        let (sender, receiver) = tokio::sync::oneshot::channel();
        window
            .dialog()
            .message(format!(
                "{} wants to {action}. Allow this operation for this page session?{network_notice}",
                target.client.origin
            ))
            .title("Allow LAN operation?")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Allow".into(),
                "Cancel".into(),
            ))
            .show(move |allowed| {
                let _ = sender.send(allowed);
            });
        let allowed = receiver.await.unwrap_or(false);
        lan_bridges()
            .lock()
            .map_err(|error| NativeEngineBridgeError::internal(error.to_string()))?
            .consent
            .finish(target.clone(), generation, allowed)?;
    }
    Ok(LanAuthorization { target, generation })
}

#[tauri::command]
pub async fn authorize_lan_server(
    window: WebviewWindow,
    url: String,
) -> Result<(), NativeEngineBridgeError> {
    authorize_lan_operation(&window, LanOperation::Connect(lan_endpoint(&url)?))
        .await?
        .require_current()
}

#[tauri::command]
pub async fn connect_lan_server(
    window: WebviewWindow,
    url: String,
    on_event: Channel<BridgeEvent>,
) -> Result<u64, NativeEngineBridgeError> {
    let endpoint = lan_endpoint(&url)?;
    let client = invoking_lan_client(&window)?;
    let request = lan_request(&endpoint, &client.origin)?;
    let target = LanTarget {
        client: client.clone(),
        operation: LanOperation::Connect(endpoint),
    };
    let (outbound, receiver) = mpsc::unbounded_channel();
    let (abort, registration) = AbortHandle::new_pair();
    let dial_abort = abort.clone();
    // Register before awaiting so navigation prevents a pending dial from becoming an active bridge.
    let id = {
        let mut state = lan_bridges()
            .lock()
            .map_err(|error| NativeEngineBridgeError::internal(error.to_string()))?;
        state.consent.require(&target)?;
        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or_else(|| NativeEngineBridgeError::internal("LAN bridge IDs exhausted"))?;
        let id = state.next_id;
        state.bridges.insert(
            id,
            LanBridge {
                client,
                handle: BridgeHandle::new(abort, outbound),
            },
        );
        id
    };
    let connection = tokio::time::timeout(Duration::from_secs(5), connect_async(request)).await;
    let socket = match connection {
        Ok(Ok((socket, _))) if !dial_abort.is_aborted() => socket,
        other => {
            remove_lan_bridge(id);
            let detail = match other {
                Err(_) => "LAN connection timed out".to_owned(),
                Ok(Err(error)) => error.to_string(),
                Ok(Ok(_)) => "LAN connection cancelled".to_owned(),
            };
            return Err(NativeEngineBridgeError::Connect { detail });
        }
    };
    tauri::async_runtime::spawn(async move {
        tokio::task::yield_now().await;
        forward_bridge(
            id,
            socket,
            receiver,
            registration,
            on_event,
            remove_lan_bridge,
        )
        .await;
    });
    Ok(id)
}

fn remove_lan_bridge(id: u64) {
    if let Ok(mut state) = lan_bridges().lock() {
        state.bridges.remove(&id);
    }
}

impl LanBridges {
    fn owned_bridge(
        &self,
        id: u64,
        client: &LanClient,
    ) -> Result<&LanBridge, NativeEngineBridgeError> {
        self.bridges
            .get(&id)
            .filter(|bridge| &bridge.client == client)
            .ok_or_else(|| NativeEngineBridgeError::UnknownBridge {
                detail: format!("LAN bridge {id} is not open for this page"),
            })
    }

    fn send(
        &self,
        id: u64,
        client: &LanClient,
        text: String,
    ) -> Result<(), NativeEngineBridgeError> {
        self.owned_bridge(id, client)?
            .handle
            .outbound()
            .send(Message::Text(text.into()))
            .map_err(|error| NativeEngineBridgeError::Send {
                detail: error.to_string(),
            })
    }

    fn close(&mut self, id: u64, client: &LanClient) -> Result<(), NativeEngineBridgeError> {
        self.owned_bridge(id, client)?;
        if let Some(bridge) = self.bridges.remove(&id) {
            bridge.handle.abort();
        }
        Ok(())
    }
}

#[tauri::command]
pub fn lan_bridge_send(
    window: WebviewWindow,
    id: u64,
    text: String,
) -> Result<(), NativeEngineBridgeError> {
    let client = invoking_lan_client(&window)?;
    lan_bridges()
        .lock()
        .map_err(|error| NativeEngineBridgeError::internal(error.to_string()))?
        .send(id, &client, text)
}

#[tauri::command]
pub fn lan_bridge_close(window: WebviewWindow, id: u64) -> Result<(), NativeEngineBridgeError> {
    let client = invoking_lan_client(&window)?;
    lan_bridges()
        .lock()
        .map_err(|error| NativeEngineBridgeError::internal(error.to_string()))?
        .close(id, &client)
}

pub(crate) fn abort_lan_bridges() {
    if let Ok(mut state) = lan_bridges().lock() {
        state.consent.clear();
        for (_, bridge) in std::mem::take(&mut state.bridges) {
            bridge.handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client() -> LanClient {
        LanClient {
            window: "main".into(),
            origin: "https://phase-rs.dev".into(),
        }
    }

    #[test]
    fn lan_consent_requires_approval_for_each_window_origin_and_operation() {
        let mut consent = LanConsent::default();
        let target = LanTarget {
            client: test_client(),
            operation: LanOperation::Connect("ws://192.168.1.2:9374/ws".into()),
        };
        assert!(consent.require(&target).is_err());
        let generation = consent.begin(&target).unwrap().unwrap();
        assert!(consent.require(&target).is_err());
        assert!(consent.begin(&target).is_err());
        consent.finish(target.clone(), generation, true).unwrap();
        consent.require(&target).unwrap();
        assert_eq!(consent.begin(&target).unwrap(), None);
        for client in [
            LanClient {
                window: "other".into(),
                ..test_client()
            },
            LanClient {
                origin: "https://preview.phase-rs.dev".into(),
                ..test_client()
            },
        ] {
            assert!(consent
                .require(&LanTarget {
                    client,
                    operation: target.operation.clone()
                })
                .is_err());
        }
        for operation in [
            LanOperation::Connect("ws://192.168.1.3:9374/ws".into()),
            LanOperation::Connect("ws://192.168.1.2:9375/ws".into()),
            LanOperation::Discover,
            LanOperation::StartHosting,
            LanOperation::StopHosting,
        ] {
            assert!(consent
                .require(&LanTarget {
                    client: test_client(),
                    operation
                })
                .is_err());
        }
        consent.clear();
        assert!(consent.require(&target).is_err());
    }

    #[test]
    fn navigation_invalidates_every_pending_operation_and_rejection_is_cached() {
        for operation in [
            LanOperation::Connect("ws://127.0.0.1:9374/ws".into()),
            LanOperation::Discover,
            LanOperation::StartHosting,
            LanOperation::StopHosting,
        ] {
            let mut consent = LanConsent::default();
            let target = LanTarget {
                client: test_client(),
                operation,
            };
            let old_generation = consent.begin(&target).unwrap().unwrap();
            consent.clear();
            let generation = consent.begin(&target).unwrap().unwrap();
            assert!(consent
                .finish(target.clone(), old_generation, true)
                .is_err());
            assert!(consent.require(&target).is_err());
            assert!(consent.finish(target.clone(), generation, false).is_err());
            assert!(consent.require(&target).is_err());
            assert!(consent.begin(&target).is_err());
            consent.clear();
            assert!(consent.begin(&target).unwrap().is_some());
        }
    }

    #[test]
    fn discovery_and_hosting_approval_never_grants_connection_access() {
        for operation in [
            LanOperation::Discover,
            LanOperation::StartHosting,
            LanOperation::StopHosting,
        ] {
            let mut consent = LanConsent::default();
            let target = LanTarget {
                client: test_client(),
                operation,
            };
            let generation = consent.begin(&target).unwrap().unwrap();
            consent.finish(target.clone(), generation, true).unwrap();
            consent.require(&target).unwrap();
            assert!(consent
                .require(&LanTarget {
                    client: test_client(),
                    operation: LanOperation::Connect("ws://127.0.0.1:9374/ws".into())
                })
                .is_err());
        }
    }

    #[test]
    fn bridge_send_and_close_require_the_owning_window_and_origin() {
        let mut state = LanBridges::default();
        let (outbound, mut receiver) = mpsc::unbounded_channel();
        let (abort, _) = AbortHandle::new_pair();
        state.bridges.insert(
            1,
            LanBridge {
                client: test_client(),
                handle: BridgeHandle::new(abort.clone(), outbound),
            },
        );
        for client in [
            LanClient {
                window: "other".into(),
                ..test_client()
            },
            LanClient {
                origin: "https://preview.phase-rs.dev".into(),
                ..test_client()
            },
        ] {
            assert!(state.send(1, &client, "forbidden".into()).is_err());
            assert!(state.close(1, &client).is_err());
            assert_eq!(state.bridges.len(), 1);
            assert!(!abort.is_aborted());
            assert!(receiver.try_recv().is_err());
        }
        state.send(1, &test_client(), "allowed".into()).unwrap();
        assert_eq!(
            receiver.try_recv().unwrap(),
            Message::Text("allowed".into())
        );
        state.close(1, &test_client()).unwrap();
        assert!(state.bridges.is_empty());
        assert!(abort.is_aborted());
    }

    #[test]
    fn lan_endpoint_accepts_only_explicit_private_or_loopback_ipv4() {
        for url in [
            "ws://10.1.2.3:1234/ws",
            "ws://172.16.0.1:9374/ws",
            "ws://192.168.1.2:80/ws",
            "ws://127.0.0.1:4321/ws",
        ] {
            assert_eq!(lan_endpoint(url).unwrap(), url);
        }
        for url in [
            "ws://8.8.8.8:1234/ws",
            "ws://0.0.0.0:1234/ws",
            "ws://224.0.0.1:1234/ws",
            "ws://169.254.1.2:1234/ws",
            "ws://172.32.0.1:1234/ws",
            "ws://localhost:1234/ws",
            "ws://2130706433:1234/ws",
            "ws://127.1:1234/ws",
            "ws://[::1]:1234/ws",
            "wss://192.168.1.2:1234/ws",
            "ws://user@192.168.1.2:1234/ws",
            "ws://192.168.1.2:1234/ws?x",
            "ws://192.168.1.2:1234/ws#x",
            "ws://192.168.1.2:1234/admin",
            "ws://192.168.1.2:0/ws",
            "ws://192.168.1.2:1234/../ws",
        ] {
            assert!(lan_endpoint(url).is_err(), "accepted {url}");
        }
        assert_eq!(
            lan_request("ws://192.168.1.2:1234/ws", "https://app.phase-rs.dev")
                .unwrap()
                .headers()[ORIGIN],
            "https://phase-rs.dev"
        );
        assert!(lan_request("ws://192.168.1.2:1234/ws", "https://untrusted.example").is_err());
    }

    #[test]
    fn solo_navigation_registry_cleanup_does_not_close_lan_bridge() {
        let (outbound, mut receiver) = mpsc::unbounded_channel();
        let (abort, _) = AbortHandle::new_pair();
        let id = {
            let mut state = lan_bridges().lock().unwrap();
            state.next_id += 1;
            let id = state.next_id;
            state.bridges.insert(
                id,
                LanBridge {
                    client: test_client(),
                    handle: BridgeHandle::new(abort, outbound),
                },
            );
            id
        };
        native_engine::abort_native_engine_bridges_on_navigation();
        lan_bridges()
            .lock()
            .unwrap()
            .send(id, &test_client(), "still open".into())
            .unwrap();
        assert_eq!(
            receiver.try_recv().unwrap(),
            Message::Text("still open".into())
        );
        lan_bridges()
            .lock()
            .unwrap()
            .close(id, &test_client())
            .unwrap();
        assert!(lan_bridges()
            .lock()
            .unwrap()
            .send(id, &test_client(), "closed".into())
            .is_err());
    }

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

        // The path is load-bearing: phase-server answers 404 on the root, and a
        // failed upgrade silently downgrades every native session to WASM.
        assert_eq!(request.uri().to_string(), "ws://127.0.0.1:43123/ws");
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
