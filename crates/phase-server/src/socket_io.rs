//! Socket I/O is independent of application handlers and snapshot encoding.
//! Application messages retain their FIFO mailbox; only transport control and
//! application Pong replies may overtake a frame that has not started writing.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use server_core::protocol::{ServerMessage, WireFormat};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::socket_control::{CloseOrigin, Control, ControlLane, Encoding, Liveness};
use crate::socket_disconnect::{Cause, Death, Disconnect};
use crate::socket_ingress::{Admission, IngressBudget, MAX_COMMANDS};
use crate::wire;

const PING_INTERVAL: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const WARN_PENDING_FRAMES: usize = 500;
const WARN_PENDING_BYTES: usize = 64 * 1024 * 1024;
static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug)]
pub(crate) enum HeartbeatInspection {
    Deferred,
    Complete,
}

/// Server-observed ingress time travels with the payload through both queues.
/// It is never taken from a client-controlled timestamp.
#[derive(Debug)]
pub(crate) struct Received<T> {
    pub(crate) message: T,
    pub(crate) received_at: Instant,
    pub(crate) admission: Admission,
    pub(crate) heartbeat: HeartbeatInspection,
}

#[derive(Debug, Default)]
struct PendingMetrics {
    frames: AtomicUsize,
    payload_bytes: AtomicUsize,
}

#[derive(Debug)]
enum WriteOperation {
    Frame(Message),
    FinishPeerClose(Option<CloseFrame>),
}

#[derive(Debug)]
struct PendingWrite {
    operation: Option<WriteOperation>,
    acknowledgement: Option<oneshot::Sender<Result<(), String>>>,
    payload_bytes: usize,
    metrics: Arc<PendingMetrics>,
    #[cfg(test)]
    flush_gate: Option<Arc<crate::socket_test_hooks::BootstrapGate>>,
}

impl PendingWrite {
    fn new(
        frame: Message,
        acknowledgement: Option<oneshot::Sender<Result<(), String>>>,
        metrics: &Arc<PendingMetrics>,
    ) -> Self {
        Self::from_operation(WriteOperation::Frame(frame), acknowledgement, metrics)
    }

    #[cfg(test)]
    fn with_flush_gate(
        mut self,
        gate: Option<Arc<crate::socket_test_hooks::BootstrapGate>>,
    ) -> Self {
        self.flush_gate = gate;
        self
    }

    fn from_operation(
        operation: WriteOperation,
        acknowledgement: Option<oneshot::Sender<Result<(), String>>>,
        metrics: &Arc<PendingMetrics>,
    ) -> Self {
        let payload_bytes = match &operation {
            WriteOperation::Frame(Message::Text(text)) => text.len(),
            WriteOperation::Frame(
                Message::Binary(bytes) | Message::Ping(bytes) | Message::Pong(bytes),
            ) => bytes.len(),
            WriteOperation::Frame(Message::Close(close))
            | WriteOperation::FinishPeerClose(close) => {
                close.as_ref().map_or(0, |close| 2 + close.reason.len())
            }
        };
        metrics.frames.fetch_add(1, Ordering::Relaxed);
        metrics
            .payload_bytes
            .fetch_add(payload_bytes, Ordering::Relaxed);
        Self {
            operation: Some(operation),
            acknowledgement,
            payload_bytes,
            metrics: metrics.clone(),
            #[cfg(test)]
            flush_gate: None,
        }
    }
}

impl Drop for PendingWrite {
    fn drop(&mut self) {
        self.metrics.frames.fetch_sub(1, Ordering::Relaxed);
        self.metrics
            .payload_bytes
            .fetch_sub(self.payload_bytes, Ordering::Relaxed);
    }
}

/// All application replies join the same typed FIFO as broadcasts. Only
/// bounded, coalesced transport/Pong controls bypass application encoding.
#[derive(Clone)]
pub(crate) struct NegotiatedSocket {
    messages: crate::socket_mailbox::Sender,
    control: ControlLane,
    death: Death,
    command_id: Option<server_core::protocol::CommandId>,
    #[cfg(test)]
    hooks: crate::socket_test_hooks::Hooks,
}

impl NegotiatedSocket {
    pub(crate) fn set_command_id(&mut self, command_id: Option<server_core::protocol::CommandId>) {
        self.command_id = command_id;
    }

    pub(crate) fn command_id(&self) -> Option<server_core::protocol::CommandId> {
        self.command_id
    }

    pub(crate) fn record_quota_discard(&self) {
        self.death.record_quota_discard();
    }

    pub(crate) fn enqueue_completion(
        &self,
        message: ServerMessage,
        command_id: Option<server_core::protocol::CommandId>,
    ) -> Result<(), mpsc::error::SendError<()>> {
        self.messages.send_completion(message, command_id)
    }

    pub(crate) fn enqueue_quota_refusal(
        &self,
        command_id: Option<server_core::protocol::CommandId>,
    ) -> Result<(), crate::socket_mailbox::RefusalError> {
        let result = self.messages.send_refusal(
            ServerMessage::ActionFailed {
                message: "Command rate quota exceeded".to_string(),
            },
            command_id,
        );
        if matches!(result, Err(crate::socket_mailbox::RefusalError::Full)) {
            self.close_for_ingress(Cause::QuotaRefusalOverflow);
        }
        result
    }

    pub(crate) fn set_wire_format(&mut self, format: Option<WireFormat>) {
        self.control.accept_wire_format(format);
    }

    pub(crate) fn uses_gzip_envelope(&self) -> bool {
        self.control.encoding() == Encoding::GzipEnvelope
    }

    pub(crate) fn enqueue_reply(
        &self,
        message: ServerMessage,
    ) -> Result<(), mpsc::error::SendError<()>> {
        self.enqueue_completion(message, self.command_id)
    }

    pub(crate) fn enqueue_pong(&self, timestamp: u64) {
        self.control.application_pong(
            timestamp,
            self.uses_gzip_envelope()
                .then_some(WireFormat::GzipEnvelopeV1),
        );
        self.control.note_application_activity();
    }

    pub(crate) async fn application_activity(&self) {
        self.control.application_activity().await;
    }

    #[cfg(test)]
    pub(crate) async fn before_bootstrap_cutover(&self) {
        self.hooks.before_bootstrap_cutover().await;
    }

    #[cfg(test)]
    pub(crate) fn bootstrap_hooks(&self) -> crate::socket_test_hooks::Hooks {
        self.hooks.clone()
    }
    pub(crate) fn probe_heartbeat(&self, frame: &Message) -> Option<u64> {
        self.control.probe_heartbeat(frame)
    }
    #[cfg(test)]
    pub(crate) fn observe_probe_work(&self, work: Arc<crate::socket_test_hooks::ParserWork>) {
        self.control.observe_probe_work(work);
    }

    pub(crate) fn ingress_overflow(&self) {
        self.close_for_ingress(Cause::IngressOverflow);
    }

    pub(crate) fn quota_discard_overflow(&self) {
        self.close_for_ingress(Cause::QuotaDiscardOverflow);
    }

    fn close_for_ingress(&self, cause: Cause) {
        self.control.close(
            Some(CloseFrame {
                code: 1013,
                reason: Default::default(),
            }),
            CloseOrigin::Server,
            || self.death.record(cause),
        );
    }
}

pub(crate) struct SocketTasks {
    death: Death,
    reader: JoinHandle<()>,
    encoder: JoinHandle<()>,
    writer: JoinHandle<()>,
    decoder: Option<JoinHandle<()>>,
}

impl SocketTasks {
    pub(crate) fn set_decoder(&mut self, decoder: JoinHandle<()>) {
        debug_assert!(self.decoder.is_none());
        self.decoder = Some(decoder);
    }

    pub(crate) fn closed(&self) -> watch::Receiver<Option<Disconnect>> {
        self.death.closed()
    }

    pub(crate) fn disconnected_at(&self) -> Option<Instant> {
        self.death.event().map(|event| event.at)
    }

    pub(crate) fn disconnect(&self) -> Option<Disconnect> {
        self.death.event()
    }

    pub(crate) async fn shutdown(mut self) {
        self.death.record(Cause::ApplicationEnd);
        if let Some(decoder) = self.decoder.take() {
            decoder.abort();
            let _ = decoder.await;
        }
        // Let the writer flush the peer's Close reply before lock-taking cleanup.
        // A dead or non-reading peer cannot delay transport teardown indefinitely.
        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
            let _ = tokio::join!(&mut self.reader, &mut self.encoder, &mut self.writer);
        })
        .await;
    }
}

impl Drop for SocketTasks {
    fn drop(&mut self) {
        self.death.record(Cause::ApplicationEnd);
        self.reader.abort();
        self.encoder.abort();
        self.writer.abort();
        if let Some(decoder) = &self.decoder {
            decoder.abort();
        }
    }
}

pub(crate) struct SocketConfig {
    pub(crate) replies: crate::socket_mailbox::Sender,
    pub(crate) metrics: Arc<crate::metrics::ServerMetrics>,
    pub(crate) executor: crate::game_executor::GameExecutor,
    #[cfg(test)]
    pub(crate) hooks: crate::socket_test_hooks::Hooks,
}

pub(crate) fn start(
    socket: WebSocket,
    mut messages: crate::socket_mailbox::Receiver,
    config: SocketConfig,
) -> (
    NegotiatedSocket,
    mpsc::Receiver<Received<Message>>,
    SocketTasks,
) {
    #[cfg(test)]
    let hooks = config.hooks;
    let executor = config.executor;
    let connection = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
    let (sink, stream) = socket.split();
    let death = Death::new(config.metrics, connection);
    let liveness = Liveness::default();
    let metrics = Arc::new(PendingMetrics::default());
    let (application, application_rx) = mpsc::channel::<PendingWrite>(1);
    let control = ControlLane::default();
    let (incoming, incoming_rx) = mpsc::channel(MAX_COMMANDS);
    let facade = NegotiatedSocket {
        messages: config.replies,
        control: control.clone(),
        death: death.clone(),
        command_id: None,
        #[cfg(test)]
        hooks: hooks.clone(),
    };

    let reader = tokio::spawn(read_socket(
        stream,
        control.clone(),
        incoming,
        death.clone(),
        liveness.clone(),
        IngressBudget::default(),
        connection,
    ));

    let encoder = tokio::spawn({
        let death = death.clone();
        let mut closed = death.closed();
        let metrics = metrics.clone();
        let control = control.clone();
        #[cfg(test)]
        let hooks = hooks.clone();
        async move {
            let mut sample = tokio::time::interval(Duration::from_secs(1));
            sample.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut warned = false;
            loop {
                let mut message = tokio::select! {
                    biased;
                    _ = closed.wait_for(Option::is_some) => break,
                    _ = sample.tick() => {
                        sample_backlog(connection, messages.queued_frames(), &metrics, &mut warned);
                        continue;
                    }
                    message = messages.recv_envelope() => match message { Some(message) => message, None => break },
                };
                #[cfg(test)]
                let flush_gate = message
                    .quota_refusal
                    .as_ref()
                    .and_then(|_| hooks.take_quota_flush());
                // Retain refusal admission through the actual socket flush,
                // not merely until the encoder has removed the mailbox entry.
                let _quota_refusal = message.quota_refusal.take();
                sample_backlog(
                    connection,
                    messages.queued_frames() + 1,
                    &metrics,
                    &mut warned,
                );
                #[cfg(test)]
                hooks.before_encode(&message.message).await;
                let encoding = encode_application(
                    &executor,
                    message,
                    control.encoding() == Encoding::GzipEnvelope,
                );
                tokio::pin!(encoding);
                let frame = loop {
                    tokio::select! {
                        biased;
                        _ = closed.wait_for(Option::is_some) => return,
                        _ = sample.tick() => sample_backlog(connection, messages.queued_frames() + 1, &metrics, &mut warned),
                        encoded = &mut encoding => break encoded,
                    }
                };
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(EncodingFailure::Serialization(error)) => {
                        // Preserve the old mailbox's serialization-error policy:
                        // skip that application frame, not the whole connection.
                        warn!(connection, %error, "WebSocket application serialization failed");
                        continue;
                    }
                    Err(EncodingFailure::Transport(error)) => {
                        warn!(connection, %error, "WebSocket application encoding failed");
                        death.record(Cause::EncodingFailure);
                        break;
                    }
                };
                let (flushed, mut completion) = oneshot::channel();
                let pending = PendingWrite::new(frame, Some(flushed), &metrics);
                #[cfg(test)]
                let pending = pending.with_flush_gate(flush_gate);
                if application.send(pending).await.is_err() {
                    break;
                }
                // Preserve the old mailbox's one-frame-at-a-time encoding:
                // eagerly serializing an entire queue would duplicate every
                // shared GameState as a full byte buffer during a slow write.
                let result = loop {
                    tokio::select! {
                        biased;
                        _ = closed.wait_for(Option::is_some) => return,
                        _ = sample.tick() => sample_backlog(connection, messages.queued_frames(), &metrics, &mut warned),
                        result = &mut completion => break result,
                    }
                };
                if !matches!(result, Ok(Ok(()))) {
                    death.record(Cause::WriteFailure);
                    break;
                }
            }
        }
    });

    let writer = tokio::spawn(write_socket(
        sink,
        application_rx,
        control,
        death.clone(),
        metrics,
        liveness,
    ));

    (
        facade,
        incoming_rx,
        SocketTasks {
            death,
            reader,
            encoder,
            writer,
            decoder: None,
        },
    )
}

async fn write_socket<S>(
    mut sink: S,
    mut application: mpsc::Receiver<PendingWrite>,
    control: ControlLane,
    death: Death,
    metrics: Arc<PendingMetrics>,
    liveness: Liveness,
) where
    S: Sink<Message, Error = axum::Error> + Unpin,
{
    enum NextWrite {
        Control(Control),
        Application(PendingWrite),
        Ping,
    }
    let mut closed = death.closed();
    let mut ping =
        tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown_deadline = None;
    loop {
        let next = tokio::select! {
            biased;
            next = control.next() => NextWrite::Control(next),
            _ = closed.wait_for(Option::is_some) => {
                // Detection publishes Close while holding the control lock.
                // Recheck after a same-poll race with the death notification.
                match control.take_ready() {
                    Some(next) => NextWrite::Control(next),
                    None => break,
                }
            }
            _ = ping.tick() => NextWrite::Ping,
            Some(pending) = application.recv() => NextWrite::Application(pending),
        };
        let mut pending = match next {
            NextWrite::Application(pending) => pending,
            NextWrite::Ping => PendingWrite::new(Message::Ping(Vec::new().into()), None, &metrics),
            NextWrite::Control(control) => match pending_control(control, &metrics).await {
                Ok(pending) => pending,
                Err(error) => {
                    warn!(%error, "WebSocket control encoding failed");
                    death.record(Cause::EncodingFailure);
                    break;
                }
            },
        };
        let operation = pending
            .operation
            .take()
            .expect("queued operation is consumed once");
        let closing = matches!(
            &operation,
            WriteOperation::Frame(Message::Close(_)) | WriteOperation::FinishPeerClose(_)
        );
        let server_ping = matches!(&operation, WriteOperation::Frame(Message::Ping(_)));
        let started = liveness.write_started();
        #[cfg(test)]
        let flush_gate = pending.flush_gate.take();
        // A control operation cannot preempt the current RFC 6455 frame.
        let write = async {
            match operation {
                WriteOperation::Frame(frame) => {
                    #[cfg(test)]
                    let mut sink =
                        crate::socket_test_hooks::FlushGateSink::new(&mut sink, flush_gate);
                    sink.send(frame).await
                }
                // Reading Close already queued the library's echo. poll_close
                // flushes it without attempting a second SendAfterClosing.
                WriteOperation::FinishPeerClose(_) => sink.close().await,
            }
        };
        tokio::pin!(write);
        let result = tokio::select! {
            biased;
            result = &mut write => result.map_err(|error| error.to_string()),
            _ = async { let _ = closed.wait_for(Option::is_some).await; } => {
                let deadline = *shutdown_deadline.get_or_insert_with(
                    || tokio::time::Instant::now() + SHUTDOWN_TIMEOUT);
                match tokio::time::timeout_at(deadline, &mut write).await {
                    Ok(result) => result.map_err(|error| error.to_string()),
                    Err(_) => Err("WebSocket transport closed during write".to_string()),
                }
            }
        };
        liveness.write_finished((server_ping && result.is_ok()).then_some(started));
        let failed = result.is_err();
        if let Err(error) = &result {
            warn!(%error, "WebSocket write failed");
            death.record(Cause::WriteFailure);
        }
        if let Some(acknowledgement) = pending.acknowledgement.take() {
            let _ = acknowledgement.send(result);
        }
        if failed || closing {
            break;
        }
    }
    death.record(Cause::ApplicationEnd);
    let deadline =
        shutdown_deadline.unwrap_or_else(|| tokio::time::Instant::now() + SHUTDOWN_TIMEOUT);
    let _ = tokio::time::timeout_at(deadline, sink.close()).await;
}

async fn pending_control(
    control: Control,
    metrics: &Arc<PendingMetrics>,
) -> Result<PendingWrite, String> {
    let operation = match control {
        Control::Close {
            frame,
            origin: CloseOrigin::Peer,
        } => WriteOperation::FinishPeerClose(frame),
        Control::Close {
            frame,
            origin: CloseOrigin::Server,
        } => WriteOperation::Frame(Message::Close(frame)),
        Control::NativePong(bytes) => WriteOperation::Frame(Message::Pong(bytes)),
        Control::ApplicationPong { timestamp, format } => {
            let envelope = matches!(format, Some(WireFormat::GzipEnvelopeV1));
            // Host activity refresh is independent; the broker remains the
            // single authority for the coalesced application heartbeat reply.
            let reply = lobby_broker::Broker::heartbeat_reply(timestamp);
            let json = serde_json::to_string(&crate::to_server_message(reply))
                .map_err(|error| error.to_string())?;
            WriteOperation::Frame(wire::encode_json_message(json, envelope).await?)
        }
    };
    Ok(PendingWrite::from_operation(operation, None, metrics))
}

async fn read_socket<S>(
    mut stream: S,
    control: ControlLane,
    incoming: mpsc::Sender<Received<Message>>,
    death: Death,
    liveness: Liveness,
    budget: IngressBudget,
    connection: u64,
) where
    S: Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    let mut closed = death.closed();
    let mut activity = liveness.reader();
    loop {
        let deadline = activity.borrow_and_update().deadline();
        let next = tokio::select! {
            biased;
            _ = closed.wait_for(Option::is_some) => break,
            // A buffered response wins over an overdue timer after starvation.
            next = stream.next() => next,
            _ = activity.changed() => continue,
            _ = response_timeout(deadline) => {
                if activity.borrow().deadline().is_some_and(|at| at <= tokio::time::Instant::now()) {
                    warn!(connection, "WebSocket heartbeat response deadline expired");
                    control.close(Some(CloseFrame { code: 1001, reason: Default::default() }), CloseOrigin::Server,
                        || death.record(Cause::ReadDeadline));
                    break;
                }
                continue;
            }
        };
        if matches!(&next, Some(Ok(_))) {
            liveness.received();
        }
        match next {
            Some(Ok(Message::Ping(bytes))) => {
                // Keep only the latest unsent native Pong payload.
                control.native_pong(bytes);
            }
            Some(Ok(Message::Pong(_))) => {}
            Some(Ok(Message::Close(close))) => {
                let code = close.as_ref().map(|frame| frame.code);
                control.close(close, CloseOrigin::Peer, || death.peer_close(code));
                break;
            }
            Some(Ok(frame @ (Message::Text(_) | Message::Binary(_)))) => {
                // Only bounded, rate-admitted probes may bypass command slots.
                // Other frames retain normal ingress and decoder rate limits.
                let encoding = control.encoding();
                let heartbeat = match encoding {
                    Encoding::Pending => HeartbeatInspection::Deferred,
                    Encoding::Plain | Encoding::GzipEnvelope => {
                        if let Some(timestamp) = control.probe_heartbeat(&frame) {
                            control.application_pong(timestamp, encoding.wire_format());
                            control.note_application_activity();
                            continue;
                        }
                        HeartbeatInspection::Complete
                    }
                };
                let bytes = match &frame {
                    Message::Text(text) => text.len(),
                    Message::Binary(bytes) => bytes.len(),
                    Message::Ping(_) | Message::Pong(_) | Message::Close(_) => unreachable!(),
                };
                let admission = match budget.admit(bytes) {
                    Ok(admission) => admission,
                    Err(limit) => {
                        warn!(connection, ?limit, usage = ?budget.usage(), "WebSocket ingress capacity exceeded");
                        control.close(
                            Some(CloseFrame {
                                code: 1013,
                                reason: Default::default(),
                            }),
                            CloseOrigin::Server,
                            || death.record(Cause::IngressOverflow),
                        );
                        break;
                    }
                };
                match incoming.try_send(Received {
                    message: frame,
                    received_at: Instant::now(),
                    admission,
                    heartbeat,
                }) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        control.close(
                            Some(CloseFrame {
                                code: 1013,
                                reason: Default::default(),
                            }),
                            CloseOrigin::Server,
                            || death.record(Cause::IngressOverflow),
                        );
                        break;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
            Some(Err(error)) => {
                warn!(connection, %error, "WebSocket read failed");
                death.read_error(&error);
                break;
            }
            None => {
                death.record(Cause::ReadEof);
                break;
            }
        }
    }
    if death.event().is_some_and(|event| {
        matches!(
            event.cause,
            Cause::IngressOverflow | Cause::QuotaRefusalOverflow | Cause::QuotaDiscardOverflow
        )
    }) {
        // Keep the read half alive while the writer flushes the local policy
        // Close: dropping TCP with pipelined input unread can erase that frame.
        // No game/session/executor ownership is taken here. An already accepted
        // mutation finishes independently; transport teardown never waits for it.
        drop(incoming);
        drop(budget);
        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
            let mut bytes = 0usize;
            for _ in 0..MAX_COMMANDS {
                // Leave room for one maximum-sized frame before polling it.
                // The upgrade enforces MAX_WS_MESSAGE_BYTES per message.
                if bytes > crate::socket_ingress::MAX_PAYLOAD_BYTES - crate::MAX_WS_MESSAGE_BYTES {
                    break;
                }
                match stream.next().await {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(Message::Text(text))) => bytes += text.len(),
                    Some(Ok(Message::Binary(data) | Message::Ping(data) | Message::Pong(data))) => {
                        bytes += data.len()
                    }
                }
            }
        })
        .await;
    }
    death.record(Cause::ApplicationEnd);
}

async fn response_timeout(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

enum EncodingFailure {
    Serialization(serde_json::Error),
    Transport(String),
}

async fn encode_application(
    executor: &crate::game_executor::GameExecutor,
    message: crate::socket_mailbox::Envelope,
    envelope: bool,
) -> Result<Message, EncodingFailure> {
    let envelope = envelope && !matches!(&message.message, ServerMessage::ServerHello { .. });
    let state_bearing = matches!(
        &message.message,
        ServerMessage::StateUpdate { .. }
            | ServerMessage::GameStarted { .. }
            | ServerMessage::AuthoritativeStateExport { .. }
    );
    let encode = move || {
        let json = serde_json::to_string(&message).map_err(EncodingFailure::Serialization)?;
        wire::encode_json_message_sync(json, envelope).map_err(EncodingFailure::Transport)
    };
    // Each connection still encodes exactly one FIFO frame at a time. Metadata
    // has an independent blocking lane; it never waits behind game CPU permits.
    let encoded = if state_bearing {
        executor.try_run(encode).await
    } else {
        tokio::task::spawn_blocking(encode).await
    };
    encoded.map_err(|error| EncodingFailure::Transport(error.to_string()))?
}

fn sample_backlog(
    connection: u64,
    unencoded_frames: usize,
    metrics: &PendingMetrics,
    warned: &mut bool,
) {
    let pending_frames = metrics.frames.load(Ordering::Relaxed) + unencoded_frames;
    let queued_wire_payload_bytes = metrics.payload_bytes.load(Ordering::Relaxed);
    let overloaded =
        pending_frames >= WARN_PENDING_FRAMES || queued_wire_payload_bytes >= WARN_PENDING_BYTES;
    if overloaded && !*warned {
        // Byte counts cover prepared wire frames (including the current write).
        // Unencoded message sizes are unknown; report their count separately
        // rather than serializing them twice or pretending they occupy zero bytes.
        warn!(
            connection,
            pending_frames,
            queued_wire_payload_bytes,
            unencoded_frames,
            "WebSocket outbound mailbox backlog"
        );
    }
    *warned = overloaded;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::socket_control::RESPONSE_TIMEOUT;

    #[tokio::test]
    async fn priority_control_waits_for_current_write_then_overtakes_unsent_frames() {
        let (gate, blocked) = oneshot::channel();
        let (started, writing) = oneshot::channel();
        let (written, mut writes) = mpsc::unbounded_channel();
        let sink = Box::pin(futures_util::sink::unfold(
            (Some(blocked), Some(started), written),
            |(mut blocked, mut started, written), frame| async move {
                if let Some(blocked) = blocked.take() {
                    let _ = started.take().unwrap().send(());
                    blocked.await.unwrap();
                }
                written.send(frame).unwrap();
                Ok::<_, axum::Error>((blocked, started, written))
            },
        ));
        let (application, application_rx) = mpsc::channel(1);
        let control = ControlLane::default();
        let death = Death::new(Arc::default(), 0);
        let metrics = Arc::new(PendingMetrics::default());
        let writer = tokio::spawn(write_socket(
            sink,
            application_rx,
            control.clone(),
            death.clone(),
            metrics.clone(),
            Liveness::default(),
        ));
        application
            .send(PendingWrite::new(Message::text("first"), None, &metrics))
            .await
            .unwrap();
        writing.await.unwrap();
        application
            .send(PendingWrite::new(Message::text("second"), None, &metrics))
            .await
            .unwrap();
        control.native_pong(axum::body::Bytes::from_static(&[42]));
        assert!(
            matches!(writes.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "control cannot preempt the frame already being written"
        );
        gate.send(()).unwrap();
        assert!(matches!(writes.recv().await, Some(Message::Text(text)) if text == "first"));
        assert!(
            matches!(writes.recv().await, Some(Message::Pong(bytes)) if bytes.as_ref() == [42])
        );
        assert!(matches!(writes.recv().await, Some(Message::Text(text)) if text == "second"));
        death.record(Cause::ApplicationEnd);
        writer.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn transport_death_finishes_a_blocked_write_without_application_cleanup() {
        let (started, writing) = oneshot::channel();
        let sink = Box::pin(futures_util::sink::unfold(
            Some(started),
            |mut started, _frame| async move {
                if let Some(started) = started.take() {
                    let _ = started.send(());
                }
                std::future::pending::<Result<Option<oneshot::Sender<()>>, axum::Error>>().await
            },
        ));
        let (application, application_rx) = mpsc::channel(1);
        let death = Death::new(Arc::default(), 0);
        let metrics = Arc::new(PendingMetrics::default());
        let writer = tokio::spawn(write_socket(
            sink,
            application_rx,
            ControlLane::default(),
            death.clone(),
            metrics.clone(),
            Liveness::default(),
        ));
        application
            .send(PendingWrite::new(Message::text("blocked"), None, &metrics))
            .await
            .unwrap();
        writing.await.unwrap();
        death.record(Cause::ReadEof);
        tokio::time::timeout(SHUTDOWN_TIMEOUT + Duration::from_millis(1), writer)
            .await
            .expect("dead transport teardown waited for a handler")
            .unwrap();
        assert_eq!(death.event().unwrap().cause, Cause::ReadEof);
    }

    #[tokio::test(start_paused = true)]
    async fn server_ping_runs_without_application_traffic() {
        let (written, mut writes) = mpsc::unbounded_channel();
        let sink = Box::pin(futures_util::sink::unfold(
            written,
            |written, frame| async move {
                written.send(frame).unwrap();
                Ok::<_, axum::Error>(written)
            },
        ));
        let (_application, application_rx) = mpsc::channel(1);
        let control = ControlLane::default();
        let death = Death::new(Arc::default(), 0);
        let writer = tokio::spawn(write_socket(
            sink,
            application_rx,
            control.clone(),
            death.clone(),
            Arc::default(),
            Liveness::default(),
        ));
        control.native_pong(axum::body::Bytes::new());
        assert!(matches!(writes.recv().await, Some(Message::Pong(_))));
        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(1), writes.recv())
                .await
                .is_err()
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(1), writes.recv())
                .await
                .expect("server Ping was not sent without application traffic"),
            Some(Message::Ping(_))
        ));
        death.record(Cause::ApplicationEnd);
        writer.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn nonreading_peer_control_flood_has_constant_retained_memory_and_close_priority() {
        let (started, writing) = oneshot::channel();
        let sink = Box::pin(futures_util::sink::unfold(
            Some(started),
            |mut started, _frame| async move {
                if let Some(started) = started.take() {
                    let _ = started.send(());
                }
                std::future::pending::<Result<Option<oneshot::Sender<()>>, axum::Error>>().await
            },
        ));
        let (application, application_rx) = mpsc::channel(1);
        let control = ControlLane::default();
        let death = Death::new(Arc::default(), 0);
        let metrics = Arc::new(PendingMetrics::default());
        let writer = tokio::spawn(write_socket(
            sink,
            application_rx,
            control.clone(),
            death.clone(),
            metrics.clone(),
            Liveness::default(),
        ));
        application
            .send(PendingWrite::new(Message::text("blocked"), None, &metrics))
            .await
            .unwrap();
        writing.await.unwrap();
        for timestamp in 0..10_000 {
            control.native_pong(axum::body::Bytes::from_static(&[7; 125]));
            control.application_pong(timestamp, None);
        }
        assert_eq!(control.pending_count(), 2);
        assert_eq!(metrics.frames.load(Ordering::Relaxed), 1);
        control.close(
            Some(CloseFrame {
                code: 1013,
                reason: Default::default(),
            }),
            CloseOrigin::Server,
            || death.record(Cause::IngressOverflow),
        );
        assert_eq!(control.pending_count(), 1);
        tokio::time::timeout(SHUTDOWN_TIMEOUT + Duration::from_millis(1), writer)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(death.event().unwrap().cause, Cause::IngressOverflow);
    }

    #[tokio::test(start_paused = true)]
    async fn flushed_ping_response_expiry_records_the_first_clock_and_close_cause() {
        let (polled, first_poll) = oneshot::channel();
        let mut polled = Some(polled);
        let stream = futures_util::stream::poll_fn(move |_| {
            if let Some(polled) = polled.take() {
                let _ = polled.send(());
            }
            std::task::Poll::Pending
        });
        let control = ControlLane::default();
        let death = Death::new(Arc::default(), 0);
        let mut closed = death.closed();
        let liveness = Liveness::default();
        let ping = liveness.write_started();
        liveness.write_finished(Some(ping));
        let (incoming, _commands) = mpsc::channel(MAX_COMMANDS);
        let reader = tokio::spawn(read_socket(
            stream,
            control.clone(),
            incoming,
            death.clone(),
            liveness,
            IngressBudget::default(),
            0,
        ));
        first_poll.await.unwrap();
        tokio::time::advance(RESPONSE_TIMEOUT + Duration::from_secs(1)).await;
        let first =
            *tokio::time::timeout(Duration::from_millis(1), closed.wait_for(Option::is_some))
                .await
                .expect("an unanswered flushed Ping must expire")
                .unwrap();
        assert!(
            matches!(control.take_ready(), Some(Control::Close { frame: Some(frame), origin: CloseOrigin::Server })
            if frame.code == 1001)
        );
        assert!(
            reader.is_finished(),
            "an idle read deadline must not linger before disconnect cleanup"
        );
        reader.await.unwrap();
        death.record(Cause::ApplicationEnd);
        assert_eq!(death.event(), first);
        assert_eq!(first.unwrap().cause, Cause::ReadDeadline);
    }

    async fn assert_silent_peer_expires(application_interval: Option<Duration>) {
        let (written, mut writes) = mpsc::unbounded_channel();
        let sink = Box::pin(futures_util::sink::unfold(
            written,
            |written, frame| async {
                written.send(frame).unwrap();
                Ok::<_, axum::Error>(written)
            },
        ));
        let control = ControlLane::default();
        let counters = Arc::new(crate::metrics::ServerMetrics::default());
        let death = Death::new(counters.clone(), 0);
        let mut closed = death.closed();
        let liveness = Liveness::default();
        let (incoming, _commands) = mpsc::channel(MAX_COMMANDS);
        let reader = tokio::spawn(read_socket(
            futures_util::stream::pending::<Result<Message, axum::Error>>(),
            control.clone(),
            incoming,
            death.clone(),
            liveness.clone(),
            IngressBudget::default(),
            0,
        ));
        let (application, application_rx) = mpsc::channel(1);
        let metrics = Arc::new(PendingMetrics::default());
        let writer = tokio::spawn(write_socket(
            sink,
            application_rx,
            control,
            death.clone(),
            metrics.clone(),
            liveness,
        ));
        assert!(matches!(writes.recv().await, Some(Message::Ping(_))));
        let first_flush = tokio::time::Instant::now();
        let mut next_application = application_interval.map(|interval| first_flush + interval);
        for _ in 1..RESPONSE_TIMEOUT.as_secs() {
            tokio::time::advance(Duration::from_secs(1)).await;
            if next_application.is_some_and(|next| next <= tokio::time::Instant::now()) {
                application
                    .send(PendingWrite::new(Message::text("state"), None, &metrics))
                    .await
                    .unwrap();
                next_application =
                    application_interval.map(|interval| tokio::time::Instant::now() + interval);
            }
            tokio::task::yield_now().await;
            assert!(death.event().is_none(), "response budget expired early");
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        let event =
            *tokio::time::timeout(Duration::from_millis(1), closed.wait_for(Option::is_some))
                .await
                .expect("outbound traffic renewed a silent peer's response budget")
                .unwrap();
        assert_eq!(event.unwrap().cause, Cause::ReadDeadline);
        assert_eq!(tokio::time::Instant::now(), first_flush + RESPONSE_TIMEOUT);
        reader.await.unwrap();
        writer.await.unwrap();
        death.record(Cause::ApplicationEnd);
        assert_eq!(counters.ws_disconnect_count(Cause::ReadDeadline), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn periodic_pings_do_not_renew_a_silent_peers_response_budget() {
        assert_silent_peer_expires(None).await;
    }

    #[tokio::test(start_paused = true)]
    async fn application_writes_do_not_renew_a_silent_peers_response_budget() {
        assert_silent_peer_expires(Some(Duration::from_secs(1))).await;
    }

    #[tokio::test(start_paused = true)]
    async fn buffered_native_response_wins_after_runtime_starvation_without_client_json_ping() {
        let liveness = Liveness::default();
        let ping = liveness.write_started();
        liveness.write_finished(Some(ping));
        tokio::time::advance(RESPONSE_TIMEOUT * 3).await;
        let (frames, mut stream_rx) = mpsc::unbounded_channel();
        frames.send(Ok(Message::Pong(Vec::new().into()))).unwrap();
        let (polled, first_poll) = oneshot::channel();
        let mut polled = Some(polled);
        let stream = futures_util::stream::poll_fn(move |cx| {
            let next = stream_rx.poll_recv(cx);
            if let Some(polled) = polled.take() {
                let _ = polled.send(());
            }
            next
        });
        let death = Death::new(Arc::default(), 0);
        let mut closed = death.closed();
        let (incoming, _commands) = mpsc::channel(MAX_COMMANDS);
        let reader = tokio::spawn(read_socket(
            stream,
            ControlLane::default(),
            incoming,
            death,
            liveness,
            IngressBudget::default(),
            0,
        ));
        first_poll.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(1), closed.wait_for(Option::is_some))
                .await
                .is_err(),
            "a buffered protocol response was discarded after scheduler starvation"
        );
        reader.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_frame_slower_than_the_response_budget_cannot_close_a_healthy_peer() {
        let (gate, blocked) = oneshot::channel();
        let (started, writing) = oneshot::channel();
        let sink = Box::pin(futures_util::sink::unfold(
            (Some(blocked), Some(started)),
            |(mut blocked, mut started), _frame| async move {
                if let Some(blocked) = blocked.take() {
                    let _ = started.take().unwrap().send(());
                    blocked.await.unwrap();
                }
                Ok::<_, axum::Error>((blocked, started))
            },
        ));
        let liveness = Liveness::default();
        // An earlier Ping was already flushed before the slow frame began.
        let ping = liveness.write_started();
        liveness.write_finished(Some(ping));
        let death = Death::new(Arc::default(), 0);
        let mut closed = death.closed();
        let (application, application_rx) = mpsc::channel(1);
        let writer = tokio::spawn(write_socket(
            sink,
            application_rx,
            ControlLane::default(),
            death.clone(),
            Arc::default(),
            liveness.clone(),
        ));
        application
            .send(PendingWrite::new(
                Message::text("slow state frame"),
                None,
                &Arc::default(),
            ))
            .await
            .unwrap();
        writing.await.unwrap();
        let stream = futures_util::stream::pending();
        let (incoming, _commands) = mpsc::channel(MAX_COMMANDS);
        let reader = tokio::spawn(read_socket(
            stream,
            ControlLane::default(),
            incoming,
            death.clone(),
            liveness.clone(),
            IngressBudget::default(),
            0,
        ));
        tokio::time::advance(RESPONSE_TIMEOUT * 3).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(1), closed.wait_for(Option::is_some))
                .await
                .is_err(),
            "a healthy slow transfer was mistaken for an unanswered heartbeat"
        );
        gate.send(()).unwrap();
        death.record(Cause::ApplicationEnd);
        writer.await.unwrap();
        reader.await.unwrap();
    }
}

#[cfg(test)]
mod socket_tests {
    use super::*;
    use crate::game_submission_tests::connect_and_hello;
    use crate::issue_4548_full_create_tests::{recv_server_message, spawn_full_mode_server};
    use server_core::protocol::ClientMessage;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    #[tokio::test]
    async fn application_ping_bypasses_an_inflight_blocked_handler() {
        let (url, server, _temp_dir, app) = spawn_full_mode_server().await;
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let mut socket = connect_and_hello(url).await;
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&ClientMessage::Ping { timestamp: 1 })
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();
            while !matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::Pong { timestamp: 1 }
            ) {}
            let held = app.lobby.lock().await;
            for message in [
                ClientMessage::SubscribeLobby,
                ClientMessage::Ping { timestamp: 2 },
            ] {
                socket
                    .send(WsMessage::Text(
                        serde_json::to_string(&message).unwrap().into(),
                    ))
                    .await
                    .unwrap();
            }
            assert!(matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::Pong { timestamp: 2 }
            ));
            drop(held);
            // Bypassing a blocked handler does not discard the application work.
            while !matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::LobbyUpdate { .. }
            ) {}
        })
        .await;
        server.abort();
        result.expect("application heartbeat waited for unrelated handler work");
    }

    #[tokio::test]
    async fn server_hello_and_ping_do_not_wait_for_player_count_publication() {
        let (url, server, _temp_dir, app) = spawn_full_mode_server().await;
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let subscribers = app.lobby_subscribers.lock().await;
            let mut socket = connect_and_hello(url).await;
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&ClientMessage::Ping { timestamp: 42 })
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();
            assert!(matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::Pong { timestamp: 42 }
            ));
            drop(subscribers);
        })
        .await;
        server.abort();
        result.expect("greeting or heartbeat waited for player-count publication");
    }
}

#[cfg(test)]
mod transport_fixup_tests {
    use super::*;
    use crate::game_submission_tests::connect_and_hello;
    use crate::issue_4548_full_create_tests::{
        recv_server_message, send_test_message, spawn_full_mode_server,
    };
    use crate::socket_ingress::MAX_PAYLOAD_BYTES;
    use server_core::protocol::ClientMessage;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    #[tokio::test]
    async fn blocked_handler_ingress_overflow_closes_explicitly_with_1013() {
        let (url, server, _directory, app) = spawn_full_mode_server().await;
        let metrics_url = format!(
            "http://{}/metrics",
            url.strip_prefix("ws://")
                .and_then(|address| address.strip_suffix("/ws"))
                .expect("test WebSocket URL")
        );
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let mut socket = connect_and_hello(url).await;
            let held = app.lobby.lock().await;
            // Unknown additive fields are valid at the serde boundary. Charge
            // their raw bytes even when the decoded unit command is smaller.
            let frame = serde_json::json!({
                "type": "SubscribeLobby",
                "padding": "x".repeat(60 * 1024),
            })
            .to_string();
            // Stop at the first frame that exceeds the retained-byte limit so
            // later writes cannot race the explicit overflow close.
            let overflow_frames = MAX_PAYLOAD_BYTES / frame.len() + 1;
            for _ in 0..overflow_frames {
                if socket
                    .send(WsMessage::Text(frame.clone().into()))
                    .await
                    .is_err()
                {
                    // Overflow can close concurrently with a pipelined send;
                    // only receiving the policy Close proves graceful teardown.
                    break;
                }
            }
            loop {
                match socket.next().await {
                    Some(Ok(WsMessage::Close(Some(close)))) => {
                        assert_eq!(u16::from(close.code), 1013);
                        let _ = socket.close(None).await;
                        break;
                    }
                    Some(Ok(_)) => {}
                    other => panic!("overflow must be an explicit retryable close: {other:?}"),
                }
            }
            // Transport death must be observable even while the application
            // handler is still waiting for its broker lock.
            let body = reqwest::get(&metrics_url)
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .text()
                .await
                .unwrap();
            assert!(
                body.contains("phase_ws_disconnects_total{cause=\"ingress_overflow\"} 1\n"),
                "scrape lost the first transport failure: {body}"
            );
            drop(held);
        })
        .await;
        server.abort();
        result.expect("a blocked handler retained an unbounded ingress backlog");
    }

    #[tokio::test]
    async fn local_capacity_close_drains_peer_ack_with_bounded_bytes() {
        for (text, expected_reads) in [
            (r#"{"type":"SubscribeLobby"}"#.to_string(), MAX_COMMANDS + 2),
            (
                "x".repeat(crate::MAX_WS_MESSAGE_BYTES),
                2 * crate::socket_ingress::MAX_PAYLOAD_BYTES / crate::MAX_WS_MESSAGE_BYTES + 1,
            ),
        ] {
            let reads = Arc::new(AtomicUsize::new(0));
            let observed = reads.clone();
            let frames =
                std::iter::repeat_with(|| Ok::<_, axum::Error>(Message::text(text.clone())))
                    .take(MAX_COMMANDS + 1)
                    .chain([Ok(Message::Close(None))]);
            let stream = futures_util::stream::iter(frames).inspect(move |_| {
                observed.fetch_add(1, Ordering::SeqCst);
            });
            let control = ControlLane::default();
            let death = Death::new(Arc::default(), 1);
            let (incoming, _commands) = mpsc::channel(MAX_COMMANDS);
            read_socket(
                stream,
                control.clone(),
                incoming,
                death.clone(),
                Liveness::default(),
                IngressBudget::default(),
                1,
            )
            .await;
            assert_eq!(death.event().unwrap().cause, Cause::IngressOverflow);
            assert_eq!(
                reads.load(Ordering::SeqCst),
                expected_reads,
                "finish a peer acknowledgement, but never drain over 1 MiB"
            );
            assert!(matches!(control.take_ready(),
                Some(Control::Close { frame: Some(frame), origin: CloseOrigin::Server })
                if frame.code == 1013));
        }
    }

    #[tokio::test]
    async fn queued_pre_hello_command_burst_still_allows_ping() {
        let (url, server, _directory, app) = spawn_full_mode_server().await;
        let hello = app.context.socket_hooks.arm_hello_accept();
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            let mut socket = connect_and_hello(url).await;
            hello.entered.notified().await;
            let held = app.lobby.lock().await;
            // Keep negotiation pending until the reader has queued the entire
            // burst. An ordered native Ping/Pong proves that without sleeps.
            for _ in 0..40 {
                socket
                    .send(WsMessage::Text(
                        serde_json::to_string(&ClientMessage::SubscribeLobby)
                            .unwrap()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&ClientMessage::Ping { timestamp: 90_001 })
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();
            socket
                .send(WsMessage::Ping(vec![19, 77].into()))
                .await
                .unwrap();
            loop {
                match socket.next().await.unwrap().unwrap() {
                    WsMessage::Pong(payload) if payload.as_ref() == [19, 77] => break,
                    WsMessage::Text(text) => {
                        let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
                        assert_ne!(
                            frame["type"], "Pong",
                            "application Ping must wait for ClientHello acceptance"
                        );
                    }
                    WsMessage::Close(close) => {
                        panic!("burst closed before hello acceptance: {close:?}")
                    }
                    _ => {}
                }
            }
            hello.release.notify_one();
            loop {
                let message: ServerMessage = match socket.next().await.unwrap().unwrap() {
                    WsMessage::Text(text) => serde_json::from_str(&text).unwrap(),
                    // Tungstenite may already have flushed its automatic Pong
                    // before the control writer sends the coalesced proof reply.
                    WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
                    frame => panic!("expected the post-hello application reply, got {frame:?}"),
                };
                if matches!(message, ServerMessage::Pong { timestamp: 90_001 }) {
                    break;
                }
            }
            drop(held);
        })
        .await;
        hello.release.notify_one();
        server.abort();
        result.expect("a normal command burst blocked the reserved heartbeat");
    }

    #[tokio::test]
    async fn compact_command_burst_still_allows_ping_while_handler_is_blocked() {
        let (url, server, _directory, app) = spawn_full_mode_server().await;
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            let mut socket = connect_and_hello(url).await;
            // Observe server-side hello acceptance before exhausting command quota.
            send_test_message(
                &mut socket,
                &ClientMessage::Ping { timestamp: 90_000 },
                false,
            )
            .await;
            loop {
                if matches!(
                    recv_server_message(&mut socket).await,
                    ServerMessage::Pong { timestamp: 90_000 }
                ) {
                    break;
                }
            }
            let held = app.lobby.lock().await;
            // Exceed decoder quota without spending the reader's Ping reserve.
            for _ in 0..40 {
                socket
                    .send(WsMessage::Text(
                        serde_json::to_string(&ClientMessage::SubscribeLobby)
                            .unwrap()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&ClientMessage::Ping { timestamp: 90_001 })
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();
            loop {
                if matches!(
                    recv_server_message(&mut socket).await,
                    ServerMessage::Pong { timestamp: 90_001 }
                ) {
                    break;
                }
            }
            drop(held);
        })
        .await;
        server.abort();
        result.expect("a normal command burst blocked the reserved heartbeat");
    }

    #[tokio::test(start_paused = true)]
    async fn reader_does_not_expire_without_a_flushed_server_ping() {
        let (_frames, mut stream_rx) = mpsc::unbounded_channel();
        let (polled, first_poll) = oneshot::channel();
        let mut polled = Some(polled);
        let stream = futures_util::stream::poll_fn(move |cx| {
            if let Some(polled) = polled.take() {
                let _ = polled.send(());
            }
            stream_rx.poll_recv(cx)
        });
        let control = ControlLane::default();
        let (incoming, _commands) = mpsc::channel(MAX_COMMANDS);
        let death = Death::new(Arc::default(), 0);
        let mut closed = death.closed();
        let reader = tokio::spawn(read_socket(
            stream,
            control,
            incoming,
            death,
            Liveness::default(),
            IngressBudget::default(),
            1,
        ));
        first_poll.await.unwrap();
        tokio::time::advance(Duration::from_secs(120)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(1), closed.wait_for(Option::is_some),)
                .await
                .is_err(),
            "a server that never flushed a Ping cannot time out its response"
        );
        reader.abort();
    }

    #[tokio::test]
    async fn mode_rejection_preserves_the_operation_failure_channel() {
        let (url, server, _directory, _app) =
            crate::issue_4548_full_create_tests::spawn_server_with_mode(
                server_core::protocol::ServerMode::LobbyOnly,
            )
            .await;
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let mut socket = connect_and_hello(url).await;
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&ClientMessage::ExportAuthoritativeState)
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();
            assert!(matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::AuthoritativeStateExportFailed { .. }
            ));
        })
        .await;
        server.abort();
        result.expect("mode rejection did not complete its operation");
    }

    type ParserSocket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    fn parser_test_frame(json: &str, format: Option<WireFormat>) -> WsMessage {
        if format.is_some() {
            let mut raw = Vec::with_capacity(json.len() + 1);
            raw.push(wire::FORMAT_RAW);
            raw.extend_from_slice(json.as_bytes());
            WsMessage::Binary(raw.into())
        } else {
            WsMessage::Text(json.to_string().into())
        }
    }

    async fn parser_test_socket(url: String, format: Option<WireFormat>) -> ParserSocket {
        use crate::issue_4548_full_create_tests::{send_test_message, test_client_hello};
        // Admission observers pace requests without application replies; disable
        // Nagle so platform delayed-ACK timers cannot stretch this test stream.
        let (mut socket, _) = tokio_tungstenite::connect_async_with_config(url, None, true)
            .await
            .unwrap();
        assert!(matches!(
            recv_server_message(&mut socket).await,
            ServerMessage::ServerHello { .. }
        ));
        send_test_message(
            &mut socket,
            &test_client_hello(format.into_iter().collect()),
            false,
        )
        .await;
        socket
    }

    async fn recv_parser_message(
        socket: &mut ParserSocket,
        format: Option<WireFormat>,
    ) -> ServerMessage {
        if format.is_some() {
            crate::issue_4548_full_create_tests::recv_enveloped_server_message(socket).await
        } else {
            recv_server_message(socket).await
        }
    }

    async fn exhaust_parser_work_quota(socket: &mut ParserSocket, format: Option<WireFormat>) {
        let ping = serde_json::json!({
            "type": "Ping", "data": { "timestamp": 7712 },
            "padding": "x".repeat(60 * 1024),
        })
        .to_string();
        // ClientHello spent the first slot. Pace the rest by a real decoder
        // reply, retaining at most one frame: ingress capacity cannot mask work.
        for _ in 1..crate::RATE_LIMIT_MESSAGES {
            socket.send(parser_test_frame(&ping, format)).await.unwrap();
            loop {
                if matches!(
                    recv_parser_message(socket, format).await,
                    ServerMessage::Pong { timestamp: 7712 },
                ) {
                    break;
                }
            }
        }
    }

    async fn exhaust_metadata_work_quota(
        socket: &mut ParserSocket,
        work: &crate::socket_test_hooks::ParserWork,
        format: Option<WireFormat>,
    ) {
        for limited in 1..=crate::RATE_LIMIT_MESSAGES as usize {
            socket
                .send(parser_test_frame(r#"{"type":"UnknownCommand"}"#, format))
                .await
                .unwrap();
            work.wait_rate_limited(limited).await;
        }
        // Raw envelopes also cross wire::decode_client_envelope's blocking
        // boundary AFTER admission. Admission does not prove parser entry yet.
        work.wait_counts((
            crate::RATE_LIMIT_MESSAGES as usize,
            crate::RATE_LIMIT_MESSAGES as usize,
        ))
        .await;
    }

    async fn expect_parser_quota_drop(
        socket: &mut ParserSocket,
        work: &crate::socket_test_hooks::ParserWork,
        expected: (usize, usize),
        limited: usize,
        format: Option<WireFormat>,
    ) {
        work.wait_rate_limited(limited).await;
        work.wait_completed(crate::RATE_LIMIT_MESSAGES as usize + limited)
            .await;
        socket
            .send(parser_test_frame(
                &serde_json::to_string(&ClientMessage::Ping { timestamp: 8813 }).unwrap(),
                format,
            ))
            .await
            .unwrap();
        loop {
            if matches!(
                recv_parser_message(socket, format).await,
                ServerMessage::Pong { timestamp: 8813 }
            ) {
                break;
            }
        }
        assert_eq!(
            work.counts(),
            expected,
            "quota exhaustion entered a full parser or metadata recovery"
        );
    }

    async fn assert_excess_large_frame_is_unparsed(format: Option<WireFormat>, excess: String) {
        let (url, server, _directory, app) = spawn_full_mode_server().await;
        let work = app.context.socket_hooks.arm_parser_work();
        let result = tokio::time::timeout(Duration::from_secs(20), async {
            let mut socket = parser_test_socket(url, format).await;
            exhaust_parser_work_quota(&mut socket, format).await;
            exhaust_metadata_work_quota(&mut socket, &work, format).await;
            let expected = (
                crate::RATE_LIMIT_MESSAGES as usize,
                crate::RATE_LIMIT_MESSAGES as usize,
            );
            assert_eq!(
                work.counts(),
                expected,
                "reach guard: both independently admitted JSON work windows are exhausted"
            );
            socket
                .send(parser_test_frame(&excess, format))
                .await
                .unwrap();
            expect_parser_quota_drop(
                &mut socket,
                &work,
                expected,
                crate::RATE_LIMIT_MESSAGES as usize + 1,
                format,
            )
            .await;
        })
        .await;
        server.abort();
        result.expect("an excess large frame was parsed or blocked a healthy heartbeat");
    }

    async fn expect_discard_budget_close(
        socket: &mut ParserSocket,
        metrics: &crate::metrics::ServerMetrics,
    ) {
        loop {
            match socket.next().await.unwrap().unwrap() {
                WsMessage::Close(Some(close)) => {
                    assert_eq!(u16::from(close.code), 1013);
                    break;
                }
                WsMessage::Text(_)
                | WsMessage::Binary(_)
                | WsMessage::Ping(_)
                | WsMessage::Pong(_) => {}
                frame => panic!("expected the bounded quota-discard close, got {frame:?}"),
            }
        }
        assert_eq!(
            metrics.ws_quota_discard_count(),
            u64::from(crate::socket_decode::MAX_QUOTA_DISCARDS_PER_WINDOW) + 1,
        );
        assert_eq!(
            metrics.ws_disconnect_count(crate::socket_disconnect::Cause::QuotaDiscardOverflow),
            1,
        );
    }

    #[tokio::test]
    async fn sustained_unparsed_drops_cannot_replenish_any_json_work_budget() {
        let (url, server, _directory, app) = spawn_full_mode_server().await;
        let work = app.context.socket_hooks.arm_parser_work();
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            let mut socket = parser_test_socket(url, None).await;
            // Malformed ordinary commands spend the SAME recovery allowance.
            for _ in 1..crate::RATE_LIMIT_MESSAGES {
                socket
                    .send(WsMessage::Text(r#"{"type":"UnknownCommand"}"#.into()))
                    .await
                    .unwrap();
                while !matches!(
                    recv_server_message(&mut socket).await,
                    ServerMessage::Error { .. }
                ) {}
            }
            let quota = crate::RATE_LIMIT_MESSAGES as usize;
            let close_at = crate::socket_decode::MAX_QUOTA_DISCARDS_PER_WINDOW as usize + 1;
            // Pace decoder completion so ingress capacity cannot hide either
            // extra parsing or the separate sustained-abuse close.
            for limited in 1..=close_at {
                socket
                    .send(WsMessage::Text(r#"{"type":"UnknownCommand"}"#.into()))
                    .await
                    .unwrap();
                work.wait_completed(quota + limited).await;
            }
            expect_discard_budget_close(&mut socket, &app.context.metrics).await;
            let (full, metadata) = work.counts();
            assert_eq!(
                work.probes(),
                crate::socket_heartbeat::PROBE_WORK_BURST as usize
            );
            assert_eq!((full, metadata), (quota, quota));
            assert!(
                work.probes() + full + metadata
                    <= crate::socket_heartbeat::PROBE_WORK_BURST as usize + quota + quota
            );
        })
        .await;
        server.abort();
        outcome.expect("sustained abuse must close without replenishing parser work");
    }

    #[tokio::test]
    async fn refunded_click_scans_keep_a_finite_combined_parser_ceiling() {
        let (url, server, _directory, app) = spawn_full_mode_server().await;
        let work = app.context.socket_hooks.arm_parser_work();
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            let mut socket = parser_test_socket(url, None).await;
            work.wait_completed(1).await;
            let quota = crate::RATE_LIMIT_MESSAGES as usize;
            // Fill ordinary work, then the independent click and nonrefunded
            // scan windows. All valid no-seat clicks use the real decoder.
            for completed in 2..=quota * 3 {
                let click = serde_json::to_string(&ClientMessage::ContestClick {
                    contest: 71,
                    seq: completed as u32,
                })
                .unwrap();
                socket.send(WsMessage::Text(click.into())).await.unwrap();
                work.wait_completed(completed).await;
                // Catch an unbounded-click control before 64 retained commands
                // could make ingress capacity, rather than parse work, reject it.
                assert!(
                    work.counts().0 <= quota * 2,
                    "typed click admission exceeded its window"
                );
            }
            assert_eq!(work.counts(), (quota * 2, quota * 2));
            assert_eq!(
                work.probes(),
                crate::socket_heartbeat::PROBE_WORK_BURST as usize
            );
            assert_eq!(
                work.probes() + work.counts().0 + work.counts().1,
                crate::socket_heartbeat::PROBE_WORK_BURST as usize + quota * 4,
                "every refunded header scan must transfer to finite click admission",
            );
            assert_eq!(app.context.metrics.ws_quota_discard_count(), quota as u64);
            let remaining =
                crate::socket_decode::MAX_QUOTA_DISCARDS_PER_WINDOW as usize + 1 - quota;
            for discarded in 1..=remaining {
                socket
                    .send(WsMessage::Text(r#"{"type":"UnknownCommand"}"#.into()))
                    .await
                    .unwrap();
                work.wait_completed(quota * 3 + discarded).await;
            }
            expect_discard_budget_close(&mut socket, &app.context.metrics).await;
            assert_eq!(
                work.counts(),
                (quota * 2, quota * 2),
                "exhausted scans stay unparsed"
            );
        })
        .await;
        server.abort();
        outcome.expect("refunded clicks escaped the combined parse or discard bound");
    }

    #[tokio::test]
    async fn excess_padded_text_is_refused_before_full_json_parse() {
        assert_excess_large_frame_is_unparsed(
            None,
            serde_json::json!({
                "type": "Ping", "data": { "timestamp": 7713 },
                "padding": "x".repeat(60 * 1024),
            })
            .to_string(),
        )
        .await;
    }

    #[tokio::test]
    async fn excess_malformed_text_skips_deserialization_and_metadata_recovery() {
        assert_excess_large_frame_is_unparsed(None, format!("{{{}", " ".repeat(60 * 1024))).await;
    }

    #[tokio::test]
    async fn excess_raw_envelopes_are_refused_before_full_json_parse() {
        assert_excess_large_frame_is_unparsed(
            Some(WireFormat::GzipEnvelopeV1),
            serde_json::json!({
                "type": "Ping", "data": { "timestamp": 7713 },
                "padding": "x".repeat(60 * 1024),
            })
            .to_string(),
        )
        .await;
    }

    #[tokio::test]
    async fn compact_refusal_inspection_is_bounded_before_metadata_recovery() {
        let (url, server, _directory, app) = spawn_full_mode_server().await;
        let work = app.context.socket_hooks.arm_parser_work();
        let result = tokio::time::timeout(Duration::from_secs(20), async {
            let mut socket = parser_test_socket(url, None).await;
            exhaust_parser_work_quota(&mut socket, None).await;
            let malformed = serde_json::json!({
                "type": "Action", "command_id": 901,
                "data": { "action": { "type": "not-an-engine-action" } },
            })
            .to_string();
            // Every rejected payload is fully received, so neither the eight
            // unflushed-refusal permits nor retained-ingress capacity can mask
            // an unbounded inspection path.
            for _ in 0..crate::RATE_LIMIT_MESSAGES {
                socket
                    .send(WsMessage::Text(malformed.clone().into()))
                    .await
                    .unwrap();
                loop {
                    if matches!(
                        recv_server_message(&mut socket).await,
                        ServerMessage::ActionFailed { .. }
                    ) {
                        break;
                    }
                }
            }
            let expected = (
                crate::RATE_LIMIT_MESSAGES as usize,
                crate::RATE_LIMIT_MESSAGES as usize,
            );
            assert_eq!(
                work.counts(),
                expected,
                "reach guard: malformed correlated refusals inspected"
            );
            socket
                .send(WsMessage::Text(malformed.into()))
                .await
                .unwrap();
            expect_parser_quota_drop(
                &mut socket,
                &work,
                expected,
                crate::RATE_LIMIT_MESSAGES as usize + 1,
                None,
            )
            .await;
        })
        .await;
        server.abort();
        result.expect("compact quota recovery bought unbounded parse work");
    }

    #[tokio::test]
    async fn oversized_text_pings_fall_through_to_the_decoder_rate_limit() {
        let (url, server, _directory, _app) = spawn_full_mode_server().await;
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let mut socket = connect_and_hello(url).await;
            for timestamp in 1..=crate::RATE_LIMIT_MESSAGES + 2 {
                let ping = serde_json::json!({
                    "type": "Ping", "data": { "timestamp": timestamp },
                    "padding": "x".repeat(crate::socket_heartbeat::MAX_HEARTBEAT_PROBE_BYTES)
                });
                socket
                    .send(WsMessage::Text(ping.to_string().into()))
                    .await
                    .unwrap();
            }
            // Decoder refusals use the ordered application lane. This reply
            // follows all admitted Pings, whose Pongs have writer priority.
            let mut delimiter = serde_json::to_value(ClientMessage::Action {
                action: engine::types::actions::GameAction::PassPriority,
            })
            .unwrap();
            delimiter
                .as_object_mut()
                .unwrap()
                .insert("command_id".to_string(), serde_json::json!(991));
            socket
                .send(WsMessage::Text(delimiter.to_string().into()))
                .await
                .unwrap();
            let mut last_pong = 0;
            loop {
                match recv_server_message(&mut socket).await {
                    ServerMessage::Pong { timestamp } => last_pong = last_pong.max(timestamp),
                    ServerMessage::ActionFailed { .. } => break,
                    _ => {}
                }
            }
            assert!(last_pong > 0, "the flood reached no admitted heartbeat");
            assert!(
                last_pong <= u64::from(crate::RATE_LIMIT_MESSAGES),
                "oversized Pings escaped decoder quota: timestamp={last_pong}"
            );
        })
        .await;
        server.abort();
        result.expect("the decoder did not delimit its heartbeat flood");
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_flood_falls_back_to_bounded_command_admission() {
        for format in [None, Some(WireFormat::GzipEnvelopeV1)] {
            let control = ControlLane::default();
            control.accept_wire_format(format);
            let death = Death::new(Arc::default(), 0);
            let budget = IngressBudget::default();
            let (incoming, mut commands) = mpsc::channel(MAX_COMMANDS);
            let count = crate::socket_heartbeat::FAST_PATH_BURST as usize + MAX_COMMANDS + 1;
            let stream = futures_util::stream::iter((0..count).map(|timestamp| {
                let json = serde_json::to_string(&ClientMessage::Ping {
                    timestamp: timestamp as u64,
                })
                .unwrap();
                let frame = if format.is_some() {
                    let mut raw = vec![wire::FORMAT_RAW];
                    raw.extend_from_slice(json.as_bytes());
                    Message::binary(raw)
                } else {
                    Message::text(json)
                };
                Ok::<_, axum::Error>(frame)
            }));
            read_socket(
                stream,
                control.clone(),
                incoming,
                death.clone(),
                Liveness::default(),
                budget.clone(),
                0,
            )
            .await;
            assert_eq!(death.event().unwrap().cause, Cause::IngressOverflow);
            assert!(matches!(
                control.take_ready(),
                Some(Control::Close { frame: Some(frame), origin: CloseOrigin::Server })
                    if frame.code == 1013
            ));
            assert_eq!(budget.usage().commands, MAX_COMMANDS);
            assert!(matches!(
                commands.try_recv().unwrap().message,
                Message::Text(_) | Message::Binary(_)
            ));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn normal_five_second_pings_stay_fast_when_the_decoder_is_blocked() {
        for format in [None, Some(WireFormat::GzipEnvelopeV1)] {
            let control = ControlLane::default();
            control.accept_wire_format(format);
            let budget = IngressBudget::default();
            let (frames, mut stream_rx) = mpsc::unbounded_channel();
            let stream = futures_util::stream::poll_fn(move |cx| stream_rx.poll_recv(cx));
            let (incoming, mut commands) = mpsc::channel(MAX_COMMANDS);
            let reader = tokio::spawn(read_socket(
                stream,
                control.clone(),
                incoming,
                Death::new(Arc::default(), 0),
                Liveness::default(),
                budget.clone(),
                0,
            ));
            for timestamp in 1..=crate::socket_heartbeat::FAST_PATH_BURST * 2 {
                let timestamp = u64::from(timestamp);
                let json = serde_json::to_string(&ClientMessage::Ping { timestamp }).unwrap();
                let frame = if format.is_some() {
                    let mut raw = vec![wire::FORMAT_RAW];
                    raw.extend_from_slice(json.as_bytes());
                    Message::binary(raw)
                } else {
                    Message::text(json)
                };
                frames.send(Ok(frame)).unwrap();
                let reply = tokio::time::timeout(Duration::from_millis(1), control.next())
                    .await
                    .expect("ordinary heartbeat waited for command decoding");
                assert!(matches!(
                    reply,
                    Control::ApplicationPong { timestamp: echoed, .. } if echoed == timestamp
                ));
                assert_eq!(budget.usage(), crate::socket_ingress::Usage::default());
                assert!(matches!(
                    commands.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ));
                tokio::time::advance(Duration::from_secs(5)).await;
            }
            reader.abort();
        }
    }

    #[tokio::test]
    async fn malformed_binary_envelopes_share_bounded_decode_admission() {
        use server_core::protocol::WireFormat;
        let (url, server, _directory, app) = spawn_full_mode_server().await;
        let work = app.context.socket_hooks.arm_parser_work();
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let mut socket = parser_test_socket(url, Some(WireFormat::GzipEnvelopeV1)).await;
            for _ in 0..2 * crate::RATE_LIMIT_MESSAGES {
                socket
                    .send(WsMessage::Binary(vec![1, 1, 2, 3].into()))
                    .await
                    .unwrap();
            }
            expect_parser_quota_drop(
                &mut socket,
                &work,
                (1, 0),
                crate::RATE_LIMIT_MESSAGES as usize + 1,
                Some(WireFormat::GzipEnvelopeV1),
            )
            .await;
        })
        .await;
        server.abort();
        result.expect("malformed binary traffic escaped bounded decode work or closed the socket");
    }

    #[tokio::test]
    async fn decoded_command_quota_refusals_echo_the_command_without_losing_heartbeats() {
        use crate::issue_4548_full_create_tests::send_test_message;
        use engine::types::interaction::{
            InteractionChoiceId, InteractionId, InteractionResponse, InteractionSubmission,
        };

        for format in [None, Some(WireFormat::GzipEnvelopeV1)] {
            let (url, server, _directory, app) = spawn_full_mode_server().await;
            let _work = app.context.socket_hooks.arm_parser_work();
            let scenario = async {
                let mut socket = parser_test_socket(url, format).await;
                for _ in 0..crate::RATE_LIMIT_MESSAGES {
                    send_test_message(
                        &mut socket,
                        &ClientMessage::SubscribeLobby,
                        format.is_some(),
                    )
                    .await;
                }
                let commands = [
                    ClientMessage::Action {
                        action: engine::types::actions::GameAction::PassPriority,
                    },
                    ClientMessage::Interaction {
                        submission: InteractionSubmission {
                            interaction_id: InteractionId("quota-interaction".to_string()),
                            response: InteractionResponse::Choose {
                                choice_id: InteractionChoiceId("quota-choice".to_string()),
                            },
                        },
                    },
                ];
                for (index, message) in commands
                    .into_iter()
                    .cycle()
                    .take(crate::socket_mailbox::MAX_PENDING_QUOTA_REFUSALS * 2 + 1)
                    .enumerate()
                {
                    let command_id = 37 + index as u64;
                    let mut frame = serde_json::to_value(message).unwrap();
                    frame["command_id"] = serde_json::json!(command_id);
                    frame["padding"] = serde_json::json!("x".repeat(300));
                    let outgoing =
                        match wire::encode_json_message_sync(frame.to_string(), format.is_some())
                            .unwrap()
                        {
                            Message::Text(text) => WsMessage::Text(text.to_string().into()),
                            Message::Binary(bytes) => WsMessage::Binary(bytes),
                            Message::Ping(_) | Message::Pong(_) | Message::Close(_) => {
                                unreachable!()
                            }
                        };
                    socket.send(outgoing).await.unwrap();
                    loop {
                        let text = match socket.next().await.unwrap().unwrap() {
                            WsMessage::Text(text) => text.to_string(),
                            WsMessage::Binary(bytes) => wire::decode_client_envelope(
                                bytes.to_vec(),
                                crate::MAX_WS_MESSAGE_BYTES,
                            )
                            .await
                            .unwrap(),
                            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
                            frame => {
                                panic!("a sequential quota refusal closed its socket: {frame:?}")
                            }
                        };
                        let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
                        if frame["type"] == "ActionFailed" {
                            assert_eq!(frame["command_id"], command_id);
                            assert_eq!(frame["data"]["message"], "Command rate quota exceeded");
                            break;
                        }
                    }
                }
                send_test_message(
                    &mut socket,
                    &ClientMessage::Ping { timestamp: 91 },
                    format.is_some(),
                )
                .await;
                loop {
                    match recv_parser_message(&mut socket, format).await {
                        ServerMessage::Pong { timestamp: 91 } => break,
                        ServerMessage::Error { message, .. } => {
                            panic!("heartbeat failed: {message}")
                        }
                        _ => {}
                    }
                }
            };
            let result = tokio::time::timeout(Duration::from_secs(10), scenario).await;
            server.abort();
            result.expect("decoded commands must receive an owned refusal");
        }
    }
}
