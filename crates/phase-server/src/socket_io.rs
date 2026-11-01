//! Socket I/O is independent of application handlers. Application messages retain
//! their FIFO mailbox; only transport control and application Pong replies may
//! overtake a frame that has not started writing.
//!
//! The complete recipient-projected Envelope is the encoding authority. Codec
//! baselines bind its FullSessionKey and state.viewer_projection, never the actor
//! or requested seat. Preparation owns a candidate separately from the delivered
//! baseline; only successful writer flush acknowledgement commits it. Blocking
//! workers move that ownership rather than copying retained projections.
//!
//! Only GameStarted/StateUpdate are codec-eligible; AuthoritativeStateExport also
//! uses game CPU admission but retains ordinary JSON. Invalid codec-state
//! provenance/serialization is fatal; legacy/non-state serialization keeps the
//! warn-and-skip policy. Non-state traffic and post-game idle retain the last
//! bounded baseline until replacement or teardown.
//!
//! A confirmed ControlLane selection switches Legacy to Codec once, at the
//! next FIFO encoding boundary. Compression remains independently negotiated.
//!
//! Sent-byte telemetry is independent of baseline commit: immutable prepared
//! classification and final payload length travel to the writer, which counts
//! each successful operation before attempting its acknowledgement. It includes
//! raw/gzip prefixes and explicit controls, not framing or automatic protocol
//! Pongs emitted by tungstenite's read path. Peer-close completion accounts the
//! declared echo payload without sending a duplicate Close. Failed operations
//! and tail-close cleanup never contribute, even if cleanup flushes buffers.
//!
//! cfg(test) resource_probe exposes opaque owned preparation/delivery tokens,
//! synchronous prepare/commit durations and numeric indexed-memory observations.
//! It uses the real writer acknowledgement with a collecting Sink; it adds no
//! production API, state field, encoding policy or instrumentation.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use engine::types::player::PlayerId;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use server_core::game_state_snapshot_wire_guard::guard_frame_bytes;
use server_core::protocol::{ServerMessage, StateCodec, WireFormat};
use server_core::FullSessionKey;
use state_codec::WireState;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::{error, warn};

use crate::metrics::{ServerMetrics, WsFrameType};
use crate::socket_control::{CloseOrigin, Control, ControlLane, Encoding, Liveness};
use crate::socket_disconnect::{Cause, Death, Disconnect};
use crate::socket_ingress::{Admission, IngressBudget, MAX_COMMANDS, MAX_INGRESS_FRAMES};
use crate::socket_mailbox::OverBound;
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
    frame_type: WsFrameType,
    metrics: Arc<PendingMetrics>,
    #[cfg(test)]
    flush_gate: Option<Arc<crate::socket_test_hooks::BootstrapGate>>,
}

impl PendingWrite {
    fn new(
        frame: Message,
        frame_type: WsFrameType,
        acknowledgement: Option<oneshot::Sender<Result<(), String>>>,
        metrics: &Arc<PendingMetrics>,
    ) -> Self {
        Self::from_operation(
            WriteOperation::Frame(frame),
            frame_type,
            acknowledgement,
            metrics,
        )
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
        frame_type: WsFrameType,
        acknowledgement: Option<oneshot::Sender<Result<(), String>>>,
        metrics: &Arc<PendingMetrics>,
    ) -> Self {
        // Final framed payload length is measured once, shared by backlog
        // occupancy and successful-operation accounting; Drop only drains backlog.
        let payload_bytes = match &operation {
            WriteOperation::Frame(frame) => payload_len(frame),
            WriteOperation::FinishPeerClose(close) => close_len(close),
        };
        metrics.frames.fetch_add(1, Ordering::Relaxed);
        metrics
            .payload_bytes
            .fetch_add(payload_bytes, Ordering::Relaxed);
        Self {
            operation: Some(operation),
            acknowledgement,
            payload_bytes,
            frame_type,
            metrics: metrics.clone(),
            #[cfg(test)]
            flush_gate: None,
        }
    }
}

fn payload_len(frame: &Message) -> usize {
    match frame {
        Message::Text(text) => text.len(),
        Message::Binary(bytes) | Message::Ping(bytes) | Message::Pong(bytes) => bytes.len(),
        Message::Close(close) => close_len(close),
    }
}

fn close_len(close: &Option<CloseFrame>) -> usize {
    close.as_ref().map_or(0, |close| 2 + close.reason.len())
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

    pub(crate) fn enqueue_refusal(
        &self,
        reason: crate::socket_mailbox::RefusalReason,
        command_id: Option<server_core::protocol::CommandId>,
    ) -> Result<(), crate::socket_mailbox::RefusalError> {
        let result = self.messages.send_refusal(
            ServerMessage::ActionFailed {
                message: reason.message().to_string(),
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

    pub(crate) fn set_state_codec(&self, codec: Option<StateCodec>) {
        self.control.accept_state_codec(codec);
    }

    pub(crate) fn state_codec(&self) -> Option<StateCodec> {
        self.control.state_codec()
    }

    pub(crate) fn try_admit_resync(
        &self,
    ) -> Result<crate::socket_mailbox::ResyncPermit, crate::socket_mailbox::RefusalError> {
        self.messages.try_admit_resync()
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
    pub(crate) max_frame_bytes: usize,
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
    let max_frame_bytes = config.max_frame_bytes;
    let connection = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
    let (sink, stream) = socket.split();
    let sent_metrics = config.metrics.clone();
    let death = Death::new(config.metrics, connection);
    let liveness = Liveness::default();
    let metrics = Arc::new(PendingMetrics::default());
    let (application, application_rx) = mpsc::channel::<PendingWrite>(1);
    let control = ControlLane::default();
    let (incoming, incoming_rx) = mpsc::channel(MAX_INGRESS_FRAMES);
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
            let mut connection_encoder = ConnectionEncoder::Legacy;
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
                #[cfg(test)]
                if matches!(
                    &message.state_encoding,
                    crate::socket_mailbox::StateEncoding::Resync(_)
                ) {
                    hooks.before_resync_encode().await;
                }
                if matches!(connection_encoder, ConnectionEncoder::Legacy)
                    && control.state_codec() == Some(StateCodec::StateCodecV1)
                {
                    connection_encoder = ConnectionEncoder::codec();
                }
                let encoding = encode_application(
                    &executor,
                    connection_encoder,
                    message,
                    control.encoding(),
                    max_frame_bytes,
                );
                tokio::pin!(encoding);
                let outcome = loop {
                    tokio::select! {
                        biased;
                        _ = closed.wait_for(Option::is_some) => return,
                        _ = sample.tick() => sample_backlog(connection, messages.queued_frames() + 1, &metrics, &mut warned),
                        encoded = &mut encoding => break encoded,
                    }
                };
                let outcome = match outcome {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        warn!(connection, %error, "WebSocket application encoding failed");
                        death.record(Cause::EncodingFailure);
                        break;
                    }
                };
                connection_encoder = outcome.encoder;
                let PreparedApplication {
                    frame,
                    frame_type,
                    candidate,
                    replaced,
                } = match outcome.prepared {
                    Ok(prepared) => prepared,
                    Err(EncodingFailure::OverBound(reason)) => {
                        error!(connection, %reason, "WebSocket application frame over the bound cannot shrink; skipped");
                        continue;
                    }
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
                if let Some(reason) = replaced {
                    warn!(connection, %reason, "WebSocket application frame over the bound; sent its replacement");
                }
                let (flushed, mut completion) = oneshot::channel();
                let pending = PendingWrite::new(frame, frame_type, Some(flushed), &metrics);
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
                connection_encoder.commit(candidate);
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
        sent_metrics,
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
    sent_metrics: Arc<ServerMetrics>,
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
            NextWrite::Ping => PendingWrite::new(
                Message::Ping(Vec::new().into()),
                WsFrameType::Control,
                None,
                &metrics,
            ),
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
        if result.is_ok() {
            // Account the completed writer operation, not queue occupancy or
            // codec commit. A dropped acknowledgement receiver cannot undo it.
            sent_metrics.record_ws_sent_payload_bytes(pending.frame_type, pending.payload_bytes);
        }
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
    Ok(PendingWrite::from_operation(
        operation,
        WsFrameType::Control,
        None,
        metrics,
    ))
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
                let admission = match budget.admit(bytes).or_else(|_| budget.admit_for_refusal()) {
                    Ok(admission) => admission,
                    // The decoder is `MAX_REFUSAL_TRANSIT` refusals behind.
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

/// Per-connection application encoder. `Codec` retains at most one delivered projection; the single prepared candidate stays outside this owner until a successful flush commits it.
enum ConnectionEncoder {
    Legacy,
    Codec(DeliveredState),
}

struct DeliveredState {
    baseline: Option<Baseline>,
}

struct Baseline {
    full_key: FullSessionKey,
    viewer: PlayerId,
    state: WireState,
}

struct PreparedApplication {
    frame: Message,
    frame_type: WsFrameType,
    candidate: Option<Baseline>,
    /// The first over-bound reason, when the frame sent replaced the envelope.
    replaced: Option<String>,
}

struct EncodingOutcome {
    encoder: ConnectionEncoder,
    prepared: Result<PreparedApplication, EncodingFailure>,
}

impl ConnectionEncoder {
    fn codec() -> Self {
        Self::Codec(DeliveredState { baseline: None })
    }

    /// Prepares the frame to send, replacing the envelope while the codec frame
    /// actually produced (Full or Delta, after gzip framing) exceeds
    /// `max_frame_bytes`; the first over-bound reason rides on the result.
    ///
    /// Each attempt derives its own baseline candidate from the envelope it
    /// encodes: a stripped `StateUpdate` yields the stripped projection the
    /// client receives, a `StateWithheld` carries no projection and so yields
    /// none, leaving the delivered baseline at the last board the client got.
    /// The envelope, and the resync permit it owns, live until this blocking
    /// preparation completes, fails or is skipped.
    fn prepare_bounded(
        &self,
        mut envelope: crate::socket_mailbox::Envelope,
        encoding: Encoding,
        max_frame_bytes: usize,
    ) -> Result<PreparedApplication, EncodingFailure> {
        let mut replaced = None;
        loop {
            let mut prepared = self.prepare_ref(&envelope, encoding)?;
            let Err(reason) = guard_frame_bytes(payload_len(&prepared.frame), max_frame_bytes)
            else {
                prepared.replaced = replaced;
                return Ok(prepared);
            };
            replaced.get_or_insert_with(|| reason.clone());
            match envelope.over_bound(reason) {
                OverBound::Retry(next) => envelope = *next,
                OverBound::Skip(reason) => return Err(EncodingFailure::OverBound(reason)),
            }
        }
    }

    /// One unbounded preparation that consumes the envelope and its resync permit.
    #[cfg(test)]
    fn prepare(
        &self,
        envelope: crate::socket_mailbox::Envelope,
        encoding: Encoding,
    ) -> Result<PreparedApplication, EncodingFailure> {
        self.prepare_ref(&envelope, encoding)
    }

    fn prepare_ref(
        &self,
        envelope: &crate::socket_mailbox::Envelope,
        encoding: Encoding,
    ) -> Result<PreparedApplication, EncodingFailure> {
        // A mailbox-local resync response forces FULL.
        let resync = envelope.state_encoding.resync();
        let is_server_hello = matches!(&envelope.message, ServerMessage::ServerHello { .. });
        let legacy_frame_type = match &envelope.message {
            ServerMessage::GameStarted { .. } => WsFrameType::GameStarted,
            ServerMessage::StateUpdate { .. } => WsFrameType::StateUpdate,
            _ => WsFrameType::Other,
        };
        let projected = match self {
            Self::Legacy => None,
            Self::Codec(delivered) => match &envelope.message {
                ServerMessage::GameStarted {
                    full_key,
                    state_revision,
                    state,
                    ..
                }
                | ServerMessage::StateUpdate {
                    full_key,
                    state_revision,
                    state,
                    ..
                } => {
                    let full_key = full_key.as_ref().ok_or_else(|| {
                        EncodingFailure::Transport("codec state is missing FullSessionKey".into())
                    })?;
                    let viewer = state.viewer_projection.ok_or_else(|| {
                        EncodingFailure::Transport(
                            "codec state is missing viewer_projection".into(),
                        )
                    })?;
                    Some((delivered, full_key, viewer, *state_revision))
                }
                _ => None,
            },
        };
        let json = serde_json::to_string(envelope).map_err(|error| {
            if projected.is_some() {
                EncodingFailure::Transport(error.to_string())
            } else {
                EncodingFailure::Serialization(error)
            }
        })?;
        let (json, candidate, frame_type) = match projected {
            Some((delivered, full_key, viewer, revision)) => {
                let state = WireState::from_json(json.as_bytes())
                    .map_err(|error| EncodingFailure::Transport(error.to_string()))?
                    .with_revision(revision);
                drop(json);
                let baseline = delivered.baseline.as_ref().filter(|baseline| {
                    baseline.full_key == *full_key
                        && baseline.viewer == viewer
                        && Some(revision) >= baseline.state.revision()
                        && !matches!(&envelope.message, ServerMessage::GameStarted { .. })
                        && resync.is_none()
                });
                let frame = state_codec::encode(baseline.map(|baseline| &baseline.state), &state)
                    .map_err(|error| EncodingFailure::Transport(error.to_string()))?;
                let frame_type = match frame.kind() {
                    state_codec::FrameKind::Full => WsFrameType::Full,
                    state_codec::FrameKind::Delta => WsFrameType::Delta,
                };
                let bytes = frame
                    .to_bytes()
                    .map_err(|error| EncodingFailure::Transport(error.to_string()))?;
                let json = String::from_utf8(bytes)
                    .map_err(|error| EncodingFailure::Transport(error.to_string()))?;
                (json, Some((full_key.clone(), viewer, state)), frame_type)
            }
            None => (json, None, legacy_frame_type),
        };
        let frame = wire::encode_json_message_sync(
            json,
            encoding == Encoding::GzipEnvelope && !is_server_hello,
        )
        .map_err(EncodingFailure::Transport)?;
        Ok(PreparedApplication {
            frame,
            frame_type,
            candidate: candidate.map(|(full_key, viewer, state)| Baseline {
                full_key,
                viewer,
                state,
            }),
            replaced: None,
        })
    }

    /// Consume authority only after the writer acknowledges a successful flush.
    fn commit(&mut self, candidate: Option<Baseline>) {
        match self {
            Self::Legacy => {}
            Self::Codec(delivered) => {
                if let Some(candidate) = candidate {
                    delivered.baseline = Some(candidate);
                }
            }
        }
    }
}

#[derive(Debug)]
enum EncodingFailure {
    Serialization(serde_json::Error),
    Transport(String),
    OverBound(String),
}

/// Encodes one envelope, replacing it while its frame exceeds `max_frame_bytes`;
/// the first over-bound reason comes back with the frame actually sent.
async fn encode_application(
    executor: &crate::game_executor::GameExecutor,
    encoder: ConnectionEncoder,
    message: crate::socket_mailbox::Envelope,
    encoding: Encoding,
    max_frame_bytes: usize,
) -> Result<EncodingOutcome, tokio::task::JoinError> {
    let state_bearing = matches!(
        &message.message,
        ServerMessage::StateUpdate { .. }
            | ServerMessage::GameStarted { .. }
            | ServerMessage::AuthoritativeStateExport { .. }
    );
    let encode = move || {
        let prepared = encoder.prepare_bounded(message, encoding, max_frame_bytes);
        EncodingOutcome { encoder, prepared }
    };
    // Each connection still encodes exactly one FIFO frame at a time. Metadata
    // has an independent blocking lane; it never waits behind game CPU permits.
    if state_bearing {
        executor.try_run(encode).await
    } else {
        tokio::task::spawn_blocking(encode).await
    }
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
mod codec_tests {
    use super::*;
    use crate::game_executor::{GameExecutor, StateUpdateSource};
    use crate::socket_mailbox::Envelope;
    use engine::game::game_object::GameObject;
    use engine::game::scenario::GameRunner;
    use engine::types::actions::GameAction;
    use engine::types::events::GameEvent;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;
    use server_core::protocol::CommandId;
    use state_codec::{Frame, FrameKind};
    use std::num::NonZeroUsize;

    pub(super) fn raw_state(object_count: u64) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.priority_player = PlayerId(0);
        // Generic pre-existing permanents, not card-specific ability fixtures.
        // Their stable projected bodies make small metadata changes DELTA-eligible.
        for index in 0..object_count {
            let id = ObjectId(state.next_object_id);
            state.next_object_id += 1;
            state.objects.insert(
                id,
                GameObject::new(
                    id,
                    CardId(index + 1),
                    PlayerId(0),
                    format!("Transport fixture permanent {index}"),
                    Zone::Battlefield,
                ),
            );
            state.battlefield.push_back(id);
        }
        state
    }

    fn key() -> FullSessionKey {
        FullSessionKey {
            game_code: "CODEC1".into(),
            generation: 1,
        }
    }

    pub(super) fn projected_update(
        state: &GameState,
        viewer: PlayerId,
        revision: u64,
        events: Vec<GameEvent>,
    ) -> ServerMessage {
        let (legal_actions, spell_costs, by_object) = engine::ai_support::legal_actions_full(state);
        let auto_pass = engine::ai_support::auto_pass_recommended(state, &legal_actions);
        let result: server_core::session::ActionResult = (
            state.clone(),
            events,
            legal_actions,
            Vec::new(),
            auto_pass,
            spell_costs,
            by_object,
        );
        let source = StateUpdateSource::from_result(&result, revision);
        let source = source
            .guarded("CODEC1")
            .expect("typed transport fixture is publishable");
        if viewer == crate::SPECTATOR_PLAYER_ID {
            crate::build_spectator_state_update_message(&key(), state, &result.1, &[], revision)
                .unwrap()
        } else {
            crate::build_state_update_message(
                &source,
                &key(),
                viewer,
                Vec::new(),
                &state.eliminated_players,
            )
        }
    }

    pub(super) fn projected_started(state: &GameState, viewer: PlayerId) -> ServerMessage {
        // Reuse the same projection/derived-field authorities as StateUpdate,
        // then build the typed GameStarted transport shape (no session fixture).
        let ServerMessage::StateUpdate {
            full_key,
            state_revision,
            state,
            events,
            legal_actions,
            auto_pass_recommended,
            end_continuous_effect_offers,
            mana_payment_shortcut_actions,
            spell_costs,
            legal_actions_by_object,
            activation_block_reasons,
            derived,
            viewer_interaction,
            rewind_targets,
            ..
        } = projected_update(state, viewer, 100, Vec::new())
        else {
            unreachable!()
        };
        ServerMessage::GameStarted {
            full_key,
            state_revision,
            state,
            your_player: viewer,
            opponent_name: None,
            player_names: vec!["First".into(), "Second".into()],
            legal_actions,
            auto_pass_recommended,
            end_continuous_effect_offers,
            mana_payment_shortcut_actions,
            spell_costs,
            legal_actions_by_object,
            activation_block_reasons,
            derived,
            viewer_interaction,
            player_token: None,
            events,
            rewind_targets,
        }
    }

    pub(super) fn envelope(message: ServerMessage, command_id: Option<CommandId>) -> Envelope {
        Envelope {
            message,
            command_id,
            quota_refusal: None,
            state_encoding: crate::socket_mailbox::StateEncoding::Automatic,
        }
    }

    fn publication_revision(envelope: &Envelope) -> u64 {
        match &envelope.message {
            ServerMessage::GameStarted { state_revision, .. }
            | ServerMessage::StateUpdate { state_revision, .. } => *state_revision,
            other => panic!("not a projected state message: {other:?}"),
        }
    }

    fn wire_state(envelope: &Envelope) -> WireState {
        WireState::from_json(&serde_json::to_vec(envelope).unwrap())
            .unwrap()
            .with_revision(publication_revision(envelope))
    }

    fn delivered(encoder: &ConnectionEncoder) -> &Baseline {
        match encoder {
            ConnectionEncoder::Codec(delivered) => delivered.baseline.as_ref().unwrap(),
            ConnectionEncoder::Legacy => panic!("codec owner required"),
        }
    }

    async fn codec_frame(frame: Message) -> Frame {
        let json = match frame {
            Message::Text(text) => text.to_string(),
            Message::Binary(bytes) => {
                wire::decode_client_envelope(bytes.to_vec(), 32 * 1024 * 1024)
                    .await
                    .unwrap()
            }
            other => panic!("not an application payload: {other:?}"),
        };
        Frame::from_bytes(json.as_bytes()).unwrap()
    }

    async fn receive(
        receiver: &mut Option<WireState>,
        frame: Message,
        expected: &[u8],
        revision: u64,
    ) -> FrameKind {
        let frame = codec_frame(frame).await;
        let decoded = state_codec::decode(receiver.as_ref(), &frame).unwrap();
        assert_eq!(decoded.as_bytes(), expected, "complete Envelope exactness");
        assert_eq!(
            decoded.revision(),
            Some(revision),
            "publication revision wins"
        );
        *receiver = Some(decoded);
        frame.kind()
    }

    async fn deliver(
        encoder: &mut ConnectionEncoder,
        receiver: &mut Option<WireState>,
        envelope: Envelope,
        encoding: Encoding,
    ) -> FrameKind {
        let expected = serde_json::to_vec(&envelope).unwrap();
        let revision = publication_revision(&envelope);
        // Drive the real worker boundary, not a test-only encoder implementation.
        let owner = std::mem::replace(encoder, ConnectionEncoder::Legacy);
        let outcome = encode_application(
            &GameExecutor::new(NonZeroUsize::MIN),
            owner,
            envelope,
            encoding,
            server_core::game_state_snapshot_wire_guard::MAX_SERVER_FRAME_BYTES,
        )
        .await
        .unwrap();
        *encoder = outcome.encoder;
        let PreparedApplication {
            frame, candidate, ..
        } = outcome.prepared.unwrap();
        assert!(
            candidate.is_some(),
            "valid projected state prepares authority"
        );
        let kind = receive(receiver, frame, &expected, revision).await;
        // The receiver only sees frames considered successfully delivered.
        encoder.commit(candidate);
        kind
    }

    #[tokio::test]
    async fn codec_reconstructs_committed_projected_envelopes() {
        let mut runner = GameRunner::from_state(raw_state(12));
        let applied = runner.act(GameAction::PassPriority).unwrap();
        assert!(
            !applied.events.is_empty(),
            "real action supplies event coverage"
        );
        for encoding in [Encoding::Plain, Encoding::GzipEnvelope] {
            for viewer in [PlayerId(1), crate::SPECTATOR_PLAYER_ID] {
                let mut encoder = ConnectionEncoder::codec();
                let mut receiver = None;
                let first = projected_update(runner.state(), viewer, 101, applied.events.clone());
                if let ServerMessage::StateUpdate {
                    state,
                    legal_actions,
                    events,
                    ..
                } = &first
                {
                    assert_ne!(
                        state.state_revision, 101,
                        "explicit revision differs from embedded"
                    );
                    assert_eq!(events, &applied.events);
                    if viewer == PlayerId(1) {
                        assert!(
                            !legal_actions.is_empty(),
                            "acting seat has engine-derived actions"
                        );
                    }
                }
                assert_eq!(
                    deliver(
                        &mut encoder,
                        &mut receiver,
                        envelope(first, Some(CommandId(7))),
                        encoding
                    )
                    .await,
                    FrameKind::Full,
                );
                for revision in [102, 103] {
                    assert_eq!(
                        deliver(
                            &mut encoder,
                            &mut receiver,
                            envelope(
                                projected_update(runner.state(), viewer, revision, Vec::new()),
                                None
                            ),
                            encoding,
                        )
                        .await,
                        FrameKind::Delta,
                    );
                }

                let mut encoder = ConnectionEncoder::codec();
                let mut receiver = None;
                assert_eq!(
                    deliver(
                        &mut encoder,
                        &mut receiver,
                        envelope(projected_started(runner.state(), viewer), None),
                        encoding
                    )
                    .await,
                    FrameKind::Full,
                );
                // A GameStarted baseline may be FullOnly inside state-codec.
                deliver(
                    &mut encoder,
                    &mut receiver,
                    envelope(
                        projected_update(runner.state(), viewer, 101, Vec::new()),
                        None,
                    ),
                    encoding,
                )
                .await;
                assert_eq!(
                    deliver(
                        &mut encoder,
                        &mut receiver,
                        envelope(
                            projected_update(runner.state(), viewer, 102, Vec::new()),
                            None
                        ),
                        encoding
                    )
                    .await,
                    FrameKind::Delta,
                );
            }
        }
    }

    #[tokio::test]
    async fn codec_same_revision_removes_command_id() {
        let raw = raw_state(12);
        let message = projected_update(&raw, PlayerId(0), 100, Vec::new());
        let mut encoder = ConnectionEncoder::codec();
        let mut receiver = None;
        for (command_id, kind) in [
            (Some(CommandId(37)), FrameKind::Full),
            (None, FrameKind::Delta),
            (Some(CommandId(38)), FrameKind::Delta),
        ] {
            assert_eq!(
                deliver(
                    &mut encoder,
                    &mut receiver,
                    envelope(message.clone(), command_id),
                    Encoding::Plain
                )
                .await,
                kind,
            );
            let decoded: serde_json::Value =
                serde_json::from_slice(receiver.as_ref().unwrap().as_bytes()).unwrap();
            assert_eq!(
                decoded
                    .get("command_id")
                    .and_then(serde_json::Value::as_u64),
                command_id.map(|id| id.0)
            );
            assert_eq!(delivered(&encoder).state.revision(), Some(100));
        }
    }

    #[tokio::test]
    async fn codec_abandoned_candidate_preserves_delivered_baseline() {
        let mut raw = raw_state(12);
        let a = envelope(projected_update(&raw, PlayerId(0), 100, Vec::new()), None);
        let expected_a = serde_json::to_vec(&a).unwrap();
        let mut encoder = ConnectionEncoder::codec();
        let mut receiver = None;
        assert_eq!(
            deliver(&mut encoder, &mut receiver, a, Encoding::Plain).await,
            FrameKind::Full
        );

        raw.players[0].life -= 1;
        let b = envelope(projected_update(&raw, PlayerId(0), 101, Vec::new()), None);
        let b = encoder.prepare(b, Encoding::Plain).unwrap();
        assert_eq!(codec_frame(b.frame).await.kind(), FrameKind::Delta);
        let candidate = b.candidate.unwrap();
        assert_ne!(candidate.state.as_bytes(), expected_a);
        assert_eq!(candidate.state.revision(), Some(101));
        assert_eq!(delivered(&encoder).state.as_bytes(), expected_a);
        assert_eq!(delivered(&encoder).state.revision(), Some(100));
        assert_eq!(delivered(&encoder).full_key, key());
        drop(candidate); // Never delivered to the independent receiver.

        // Abandoned key/view replacements also leave the delivered binding intact.
        let mut switched = projected_update(&raw, PlayerId(0), 102, Vec::new());
        if let ServerMessage::StateUpdate {
            full_key, state, ..
        } = &mut switched
        {
            full_key.as_mut().unwrap().generation += 1;
            state.viewer_projection = Some(PlayerId(1));
        }
        let switched = encoder
            .prepare(envelope(switched, None), Encoding::Plain)
            .unwrap();
        assert_eq!(codec_frame(switched.frame).await.kind(), FrameKind::Full);
        let switched = switched.candidate.unwrap();
        assert_ne!(switched.full_key, delivered(&encoder).full_key);
        assert_ne!(switched.viewer, delivered(&encoder).viewer);
        drop(switched);
        assert_eq!(delivered(&encoder).full_key, key());
        assert_eq!(delivered(&encoder).viewer, PlayerId(0));
        assert_eq!(delivered(&encoder).state.as_bytes(), expected_a);

        for revision in [102, 103] {
            raw.players[1].life -= 1;
            assert_eq!(
                deliver(
                    &mut encoder,
                    &mut receiver,
                    envelope(
                        projected_update(&raw, PlayerId(0), revision, Vec::new()),
                        None
                    ),
                    Encoding::Plain
                )
                .await,
                FrameKind::Delta,
            );
            assert_eq!(delivered(&encoder).state.revision(), Some(revision));
            assert_eq!(
                delivered(&encoder).state.as_bytes(),
                receiver.as_ref().unwrap().as_bytes()
            );
        }

        let mut empty = ConnectionEncoder::codec();
        let abandoned = empty
            .prepare(
                envelope(projected_update(&raw, PlayerId(0), 104, Vec::new()), None),
                Encoding::Plain,
            )
            .unwrap();
        assert!(abandoned.candidate.is_some());
        drop(abandoned);
        let mut fresh_receiver = None;
        assert_eq!(
            deliver(
                &mut empty,
                &mut fresh_receiver,
                envelope(projected_update(&raw, PlayerId(0), 105, Vec::new()), None),
                Encoding::Plain
            )
            .await,
            FrameKind::Full
        );
    }

    #[tokio::test]
    async fn codec_binding_changes_and_revision_regression_force_full() {
        let raw = raw_state(12);
        #[derive(Debug)]
        enum Axis {
            GameCode,
            Generation,
            Seat,
            Spectator,
            Revision,
        }
        for axis in [
            Axis::GameCode,
            Axis::Generation,
            Axis::Seat,
            Axis::Spectator,
            Axis::Revision,
        ] {
            let mut encoder = ConnectionEncoder::codec();
            let mut receiver = None;
            deliver(
                &mut encoder,
                &mut receiver,
                envelope(projected_update(&raw, PlayerId(0), 100, Vec::new()), None),
                Encoding::Plain,
            )
            .await;
            assert_eq!(
                deliver(
                    &mut encoder,
                    &mut receiver,
                    envelope(projected_update(&raw, PlayerId(0), 101, Vec::new()), None),
                    Encoding::Plain
                )
                .await,
                FrameKind::Delta
            );
            // Synthetic transport boundary: change exactly one provenance axis,
            // not all fields in a newly computed recipient projection.
            let mut message = projected_update(&raw, PlayerId(0), 101, Vec::new());
            if let ServerMessage::StateUpdate {
                full_key,
                state,
                state_revision,
                ..
            } = &mut message
            {
                match axis {
                    Axis::GameCode => full_key.as_mut().unwrap().game_code = "CODEC2".into(),
                    Axis::Generation => full_key.as_mut().unwrap().generation += 1,
                    Axis::Seat => state.viewer_projection = Some(PlayerId(1)),
                    Axis::Spectator => state.viewer_projection = Some(crate::SPECTATOR_PLAYER_ID),
                    Axis::Revision => *state_revision = 99,
                }
            }
            let candidate = envelope(message.clone(), None);
            assert_eq!(
                state_codec::encode(Some(&delivered(&encoder).state), &wire_state(&candidate))
                    .unwrap()
                    .kind(),
                FrameKind::Delta,
                "{axis:?}: codec-alone control must reach DELTA on the exact pair",
            );
            receiver = None;
            assert_eq!(
                deliver(&mut encoder, &mut receiver, candidate, Encoding::Plain).await,
                FrameKind::Full,
                "{axis:?}"
            );
            if let ServerMessage::StateUpdate { state_revision, .. } = &mut message {
                *state_revision += 1;
            }
            assert_eq!(
                deliver(
                    &mut encoder,
                    &mut receiver,
                    envelope(message, None),
                    Encoding::Plain
                )
                .await,
                FrameKind::Delta,
                "{axis:?}: committed replacement supports later DELTA"
            );
        }

        let mut started = projected_started(&raw, PlayerId(0));
        if let ServerMessage::GameStarted { your_player, .. } = &mut started {
            *your_player = PlayerId(1); // Deliberately disagrees with projected marker.
        }
        let mut encoder = ConnectionEncoder::codec();
        let prepared = encoder
            .prepare(envelope(started, None), Encoding::Plain)
            .unwrap();
        assert_eq!(prepared.candidate.as_ref().unwrap().viewer, PlayerId(0));
        assert_ne!(prepared.candidate.as_ref().unwrap().viewer, PlayerId(1));
        encoder.commit(prepared.candidate);
        assert_eq!(delivered(&encoder).viewer, PlayerId(0));
    }

    #[tokio::test]
    async fn codec_requires_projected_key_and_viewer() {
        let raw = raw_state(12);
        let mut encoder = ConnectionEncoder::codec();
        let mut receiver = None;
        deliver(
            &mut encoder,
            &mut receiver,
            envelope(projected_update(&raw, PlayerId(0), 100, Vec::new()), None),
            Encoding::Plain,
        )
        .await;
        let expected_baseline = delivered(&encoder).state.as_bytes().to_vec();
        #[derive(Clone, Copy)]
        enum MissingProvenance {
            FullSessionKey,
            ViewerProjection,
        }
        for valid in [
            projected_update(&raw, PlayerId(0), 101, Vec::new()),
            projected_started(&raw, PlayerId(0)),
        ] {
            for missing in [
                MissingProvenance::FullSessionKey,
                MissingProvenance::ViewerProjection,
            ] {
                let mut invalid = valid.clone();
                match &mut invalid {
                    ServerMessage::StateUpdate {
                        full_key, state, ..
                    }
                    | ServerMessage::GameStarted {
                        full_key, state, ..
                    } => match missing {
                        MissingProvenance::FullSessionKey => *full_key = None,
                        MissingProvenance::ViewerProjection => state.viewer_projection = None,
                    },
                    _ => unreachable!(),
                }
                let invalid = envelope(invalid, None);
                let expected_legacy =
                    wire::encode_json_message_sync(serde_json::to_string(&invalid).unwrap(), false)
                        .unwrap();
                let legacy = ConnectionEncoder::Legacy
                    .prepare(envelope(invalid.message.clone(), None), Encoding::Plain)
                    .unwrap();
                assert_eq!(legacy.frame, expected_legacy);
                assert!(legacy.candidate.is_none());
                let outcome = encode_application(
                    &GameExecutor::new(NonZeroUsize::MIN),
                    encoder,
                    invalid,
                    Encoding::Plain,
                    server_core::game_state_snapshot_wire_guard::MAX_SERVER_FRAME_BYTES,
                )
                .await
                .unwrap();
                encoder = outcome.encoder;
                match outcome.prepared {
                    Err(EncodingFailure::Transport(error)) => assert!(
                        error.contains(match missing {
                            MissingProvenance::FullSessionKey => "FullSessionKey",
                            MissingProvenance::ViewerProjection => "viewer_projection",
                        }),
                        "missing provenance diagnostic: {error}",
                    ),
                    _ => panic!("missing native provenance must be fatal on codec-state lane"),
                }
                assert_eq!(delivered(&encoder).state.as_bytes(), expected_baseline);
                assert_eq!(delivered(&encoder).full_key, key());
                assert_eq!(delivered(&encoder).viewer, PlayerId(0));
            }
            let prepared = encoder
                .prepare(envelope(valid, None), Encoding::Plain)
                .unwrap();
            assert!(
                prepared.candidate.is_some(),
                "valid sibling reaches codec preparation"
            );
            let frame = codec_frame(prepared.frame).await;
            let decoded = state_codec::decode(receiver.as_ref(), &frame).unwrap();
            assert_eq!(
                decoded.as_bytes(),
                prepared.candidate.as_ref().unwrap().state.as_bytes()
            );
            // No commit: keep the shared delivered control for the next variant.
        }
    }

    async fn bounded(
        encoder: &mut ConnectionEncoder,
        message: Envelope,
        encoding: Encoding,
        bound: usize,
    ) -> PreparedApplication {
        let owner = std::mem::replace(encoder, ConnectionEncoder::Legacy);
        let outcome = encode_application(
            &GameExecutor::new(NonZeroUsize::MIN),
            owner,
            message,
            encoding,
            bound,
        )
        .await
        .unwrap();
        *encoder = outcome.encoder;
        outcome.prepared.unwrap()
    }

    fn presentation_update(raw: &GameState, revision: u64) -> ServerMessage {
        use engine::types::log::{GameLogEntry, LogCategory, LogSegment};

        let events = vec![
            GameEvent::PriorityPassed {
                player_id: PlayerId(0),
            };
            512
        ];
        let mut update = projected_update(raw, PlayerId(0), revision, events);
        let ServerMessage::StateUpdate {
            events,
            log_entries,
            ..
        } = &mut update
        else {
            panic!("projected update required")
        };
        assert_eq!(events.len(), 512);
        *log_entries = vec![GameLogEntry {
            seq: 1,
            turn: 1,
            phase: Phase::PreCombatMain,
            category: LogCategory::Game,
            segments: vec![LogSegment::Text("bounded presentation fixture".repeat(512))],
            presentation: Default::default(),
        }];
        update
    }

    fn strip_presentation(message: &mut ServerMessage) {
        let ServerMessage::StateUpdate {
            events,
            log_entries,
            ..
        } = message
        else {
            panic!("projected update required")
        };
        assert!(!events.is_empty(), "stripping must remove events");
        assert!(!log_entries.is_empty(), "stripping must remove log entries");
        events.clear();
        log_entries.clear();
    }

    async fn assert_codec_bound_uses_wire_frame(encoding: Encoding) {
        let mut raw = raw_state(128);
        let mut encoder = ConnectionEncoder::codec();
        let mut receiver = None;
        deliver(
            &mut encoder,
            &mut receiver,
            envelope(projected_update(&raw, PlayerId(0), 100, Vec::new()), None),
            encoding,
        )
        .await;
        raw.players[0].life -= 1;
        let message = projected_update(&raw, PlayerId(0), 101, Vec::new());
        let next = envelope(message.clone(), None);
        let expected = serde_json::to_vec(&next).unwrap();
        let delta = encoder.prepare_ref(&next, encoding).unwrap();
        let full = ConnectionEncoder::codec()
            .prepare_ref(&next, encoding)
            .unwrap();
        let legacy = ConnectionEncoder::Legacy
            .prepare_ref(&next, encoding)
            .unwrap();
        assert_eq!(
            codec_frame(delta.frame.clone()).await.kind(),
            FrameKind::Delta
        );
        assert_eq!(
            codec_frame(full.frame.clone()).await.kind(),
            FrameKind::Full
        );
        let small = payload_len(&delta.frame);
        let large = payload_len(&full.frame).min(payload_len(&legacy.frame));
        assert!(
            small + 1 < large,
            "wire sizes must separate Delta from Full/legacy"
        );
        let bound = small + 1;
        if encoding == Encoding::GzipEnvelope {
            assert!(matches!(delta.frame, Message::Binary(_)));
            assert!(matches!(full.frame, Message::Binary(_)));
            assert!(
                codec_frame(delta.frame.clone())
                    .await
                    .to_bytes()
                    .unwrap()
                    .len()
                    > bound,
                "gzip bound must reject the uncompressed codec frame"
            );
        }
        let prepared = bounded(&mut encoder, next, encoding, bound).await;
        assert_eq!(
            prepared.frame, delta.frame,
            "bounded preparation must preserve fitting Delta bytes"
        );
        assert!(
            prepared.replaced.is_none(),
            "fitting Delta must not be replaced"
        );
        assert_eq!(
            receive(&mut receiver, prepared.frame, &expected, 101).await,
            FrameKind::Delta
        );
        encoder.commit(prepared.candidate);
        let mut fresh = ConnectionEncoder::codec();
        let replaced = bounded(&mut fresh, envelope(message, None), encoding, bound).await;
        assert!(
            replaced.replaced.is_some(),
            "oversized Full must be replaced"
        );
        assert!(payload_len(&replaced.frame) <= bound);
        let Message::Text(text) = (match replaced.frame {
            Message::Binary(bytes) => Message::Text(
                wire::decode_client_envelope(bytes.to_vec(), 32 * 1024 * 1024)
                    .await
                    .unwrap()
                    .into(),
            ),
            other => other,
        }) else {
            panic!("application frame required")
        };
        assert!(matches!(
            serde_json::from_str::<ServerMessage>(&text).unwrap(),
            ServerMessage::StateWithheld { .. }
        ));
        assert!(replaced.candidate.is_none());
    }

    #[tokio::test]
    async fn codec_bound_plain_uses_encoded_frame() {
        assert_codec_bound_uses_wire_frame(Encoding::Plain).await;
    }

    #[tokio::test]
    async fn codec_bound_gzip_uses_compressed_frame() {
        assert_codec_bound_uses_wire_frame(Encoding::GzipEnvelope).await;
    }

    #[tokio::test]
    async fn codec_bound_stripped_candidate_matches_receiver() {
        let mut raw = raw_state(12);
        let mut encoder = ConnectionEncoder::codec();
        let mut receiver = None;
        deliver(
            &mut encoder,
            &mut receiver,
            envelope(projected_update(&raw, PlayerId(0), 100, Vec::new()), None),
            Encoding::Plain,
        )
        .await;
        raw.players[0].life -= 1;
        let message = presentation_update(&raw, 101);
        let mut stripped = message.clone();
        strip_presentation(&mut stripped);
        let stripped = envelope(stripped, Some(CommandId(8)));
        let expected = serde_json::to_vec(&stripped).unwrap();
        let next = envelope(message, Some(CommandId(8)));
        let original = encoder.prepare_ref(&next, Encoding::Plain).unwrap();
        let fitting = encoder.prepare_ref(&stripped, Encoding::Plain).unwrap();
        let bound = payload_len(&fitting.frame);
        assert!(
            payload_len(&original.frame) > bound,
            "strip retry reach guard"
        );
        let prepared = bounded(&mut encoder, next, Encoding::Plain, bound).await;
        assert!(prepared.replaced.is_some(), "presentation must be stripped");
        assert_eq!(
            prepared.candidate.as_ref().unwrap().state.as_bytes(),
            expected,
            "retry candidate must be the stripped projection"
        );
        receive(&mut receiver, prepared.frame, &expected, 101).await;
        encoder.commit(prepared.candidate);
        assert_eq!(
            delivered(&encoder).state.as_bytes(),
            receiver.as_ref().unwrap().as_bytes()
        );
        let received: ServerMessage =
            serde_json::from_slice(receiver.as_ref().unwrap().as_bytes()).unwrap();
        let ServerMessage::StateUpdate {
            events,
            log_entries,
            ..
        } = received
        else {
            panic!("stripped StateUpdate required")
        };
        assert!(events.is_empty());
        assert!(log_entries.is_empty());
        raw.players[1].life -= 1;
        assert_eq!(
            deliver(
                &mut encoder,
                &mut receiver,
                envelope(projected_update(&raw, PlayerId(0), 102, Vec::new()), None),
                Encoding::Plain
            )
            .await,
            FrameKind::Delta
        );
    }

    #[tokio::test]
    async fn codec_bound_withheld_preserves_delivered_baseline() {
        let mut raw = raw_state(12);
        let mut encoder = ConnectionEncoder::codec();
        let mut receiver = None;
        deliver(
            &mut encoder,
            &mut receiver,
            envelope(projected_update(&raw, PlayerId(0), 100, Vec::new()), None),
            Encoding::Plain,
        )
        .await;
        let before = receiver.as_ref().unwrap().clone();
        let before_key = delivered(&encoder).full_key.clone();
        let before_viewer = delivered(&encoder).viewer;
        let notice = envelope(
            ServerMessage::StateWithheld {
                full_key: Some(key()),
                state_revision: 101,
                withheld: server_core::protocol::WithheldFrame::StateUpdate,
                reason: crate::socket_mailbox::STATE_UPDATE_WITHHELD.into(),
            },
            None,
        );
        let bound = payload_len(&encoder.prepare_ref(&notice, Encoding::Plain).unwrap().frame);
        let large = raw_state(128);
        let message = presentation_update(&large, 101);
        let mut stripped = message.clone();
        strip_presentation(&mut stripped);
        assert!(
            payload_len(
                &encoder
                    .prepare_ref(&envelope(stripped, None), Encoding::Plain)
                    .unwrap()
                    .frame
            ) > bound,
            "even stripped state must exceed the notice bound"
        );
        let prepared = bounded(
            &mut encoder,
            envelope(message, None),
            Encoding::Plain,
            bound,
        )
        .await;
        assert!(prepared.replaced.is_some(), "withheld retry reach guard");
        let Message::Text(text) = prepared.frame else {
            panic!("plain notice required")
        };
        assert_eq!(text.as_str(), serde_json::to_string(&notice).unwrap());
        assert!(
            prepared.candidate.is_none(),
            "withheld frame cannot advance the baseline"
        );
        encoder.commit(prepared.candidate);
        assert_eq!(delivered(&encoder).state.as_bytes(), before.as_bytes());
        assert_eq!(delivered(&encoder).state.revision(), before.revision());
        assert_eq!(delivered(&encoder).full_key, before_key);
        assert_eq!(delivered(&encoder).viewer, before_viewer);
        assert_eq!(receiver.as_ref().unwrap().as_bytes(), before.as_bytes());
        raw.players[1].life -= 1;
        assert_eq!(
            deliver(
                &mut encoder,
                &mut receiver,
                envelope(projected_update(&raw, PlayerId(0), 102, Vec::new()), None),
                Encoding::Plain
            )
            .await,
            FrameKind::Delta
        );
    }

    #[tokio::test]
    async fn codec_bound_started_withheld_creates_no_baseline() {
        let mut encoder = ConnectionEncoder::codec();
        let notice = envelope(
            ServerMessage::StateWithheld {
                full_key: Some(key()),
                state_revision: 100,
                withheld: server_core::protocol::WithheldFrame::GameStarted {
                    your_player: PlayerId(0),
                },
                reason: crate::socket_mailbox::GAME_STARTED_WITHHELD.into(),
            },
            None,
        );
        let bound = payload_len(&encoder.prepare_ref(&notice, Encoding::Plain).unwrap().frame);
        let started = envelope(projected_started(&raw_state(12), PlayerId(0)), None);
        assert!(
            payload_len(
                &encoder
                    .prepare_ref(&started, Encoding::Plain)
                    .unwrap()
                    .frame
            ) > bound
        );
        let prepared = bounded(&mut encoder, started, Encoding::Plain, bound).await;
        assert!(
            prepared.replaced.is_some(),
            "GameStarted replacement reach guard"
        );
        let Message::Text(text) = prepared.frame else {
            panic!("plain notice required")
        };
        assert_eq!(text.as_str(), serde_json::to_string(&notice).unwrap());
        assert!(
            prepared.candidate.is_none(),
            "withheld bootstrap cannot create a baseline"
        );
        encoder.commit(prepared.candidate);
        assert!(matches!(
            encoder,
            ConnectionEncoder::Codec(DeliveredState { baseline: None })
        ));
    }

    #[tokio::test]
    async fn codec_bound_skip_keeps_codec_baseline_and_continues() {
        let mut raw = raw_state(12);
        let mut encoder = ConnectionEncoder::codec();
        let mut receiver = None;
        deliver(
            &mut encoder,
            &mut receiver,
            envelope(projected_update(&raw, PlayerId(0), 100, Vec::new()), None),
            Encoding::Plain,
        )
        .await;
        let before_state = delivered(&encoder).state.as_bytes().to_vec();
        let before_revision = delivered(&encoder).state.revision();
        let before_key = delivered(&encoder).full_key.clone();
        let before_viewer = delivered(&encoder).viewer;
        assert_eq!(receiver.as_ref().unwrap().as_bytes(), before_state);

        raw.players[0].life -= 1;
        let (sender, _mailbox) = crate::socket_mailbox::channel();
        let mut next = envelope(projected_update(&raw, PlayerId(0), 101, Vec::new()), None);
        next.state_encoding =
            crate::socket_mailbox::StateEncoding::Resync(sender.try_admit_resync().unwrap());
        // Smaller than every frame, including the StateWithheld replacement, so
        // preparation terminates on the terminal SKIP path the writer turns
        // into a logged `continue`.
        let owner = std::mem::replace(&mut encoder, ConnectionEncoder::Legacy);
        let outcome = encode_application(
            &GameExecutor::new(NonZeroUsize::MIN),
            owner,
            next,
            Encoding::Plain,
            1,
        )
        .await
        .unwrap();
        encoder = outcome.encoder;
        match outcome.prepared {
            Err(EncodingFailure::OverBound(reason)) => assert!(
                !reason.is_empty(),
                "terminal skip must carry its over-bound reason"
            ),
            Err(other) => {
                panic!("over-bound state must terminate on the skip path: {other:?}")
            }
            Ok(_) => panic!("over-bound state must terminate on the skip path"),
        }
        // The writer restores `outcome.encoder` and continues: the connection
        // stays on the codec lane with the last delivered baseline untouched,
        // because the skipped envelope never reached `commit`.
        assert!(
            matches!(encoder, ConnectionEncoder::Codec(_)),
            "skip must keep the connection on the codec lane"
        );
        assert_eq!(delivered(&encoder).state.as_bytes(), before_state);
        assert_eq!(delivered(&encoder).state.revision(), before_revision);
        assert_eq!(delivered(&encoder).full_key, before_key);
        assert_eq!(delivered(&encoder).viewer, before_viewer);
        assert_eq!(receiver.as_ref().unwrap().as_bytes(), before_state);
        drop(
            sender
                .try_admit_resync()
                .expect("skipped over-bound frame releases resync admission"),
        );

        raw.players[1].life -= 1;
        assert_eq!(
            deliver(
                &mut encoder,
                &mut receiver,
                envelope(projected_update(&raw, PlayerId(0), 102, Vec::new()), None),
                Encoding::Plain
            )
            .await,
            FrameKind::Delta,
            "next fitting state must delta against the pre-skip baseline"
        );
        assert_eq!(delivered(&encoder).state.revision(), Some(102));
    }

    #[tokio::test]
    async fn codec_bound_resync_retry_stays_full_and_releases_permit() {
        let raw = raw_state(12);
        let mut encoder = ConnectionEncoder::codec();
        let mut receiver = None;
        deliver(
            &mut encoder,
            &mut receiver,
            envelope(projected_update(&raw, PlayerId(0), 100, Vec::new()), None),
            Encoding::Plain,
        )
        .await;
        let (sender, _mailbox) = crate::socket_mailbox::channel();
        let message = presentation_update(&raw, 101);
        let mut stripped = message.clone();
        strip_presentation(&mut stripped);
        let stripped = envelope(stripped, None);
        let expected = serde_json::to_vec(&stripped).unwrap();
        let full = ConnectionEncoder::codec()
            .prepare_ref(&stripped, Encoding::Plain)
            .unwrap();
        assert_eq!(
            codec_frame(full.frame.clone()).await.kind(),
            FrameKind::Full
        );
        assert_eq!(
            codec_frame(
                encoder
                    .prepare_ref(&stripped, Encoding::Plain)
                    .unwrap()
                    .frame
            )
            .await
            .kind(),
            FrameKind::Delta
        );
        let bound = payload_len(&full.frame);
        let mut next = envelope(message.clone(), None);
        next.state_encoding =
            crate::socket_mailbox::StateEncoding::Resync(sender.try_admit_resync().unwrap());
        assert!(payload_len(&encoder.prepare_ref(&next, Encoding::Plain).unwrap().frame) > bound);
        let prepared = bounded(&mut encoder, next, Encoding::Plain, bound).await;
        assert!(
            prepared.replaced.is_some(),
            "resync strip retry reach guard"
        );
        assert_eq!(
            receive(&mut receiver, prepared.frame, &expected, 101).await,
            FrameKind::Full,
            "stripped resync must retain its forced Full hint"
        );
        drop(
            sender
                .try_admit_resync()
                .expect("bounded retry releases resync admission"),
        );
        encoder.commit(prepared.candidate);

        let notice = envelope(
            ServerMessage::StateWithheld {
                full_key: Some(key()),
                state_revision: 101,
                withheld: server_core::protocol::WithheldFrame::StateUpdate,
                reason: crate::socket_mailbox::STATE_UPDATE_WITHHELD.into(),
            },
            None,
        );
        let bound = payload_len(&encoder.prepare_ref(&notice, Encoding::Plain).unwrap().frame);
        let mut next = envelope(message, None);
        next.state_encoding =
            crate::socket_mailbox::StateEncoding::Resync(sender.try_admit_resync().unwrap());
        let prepared = bounded(&mut encoder, next, Encoding::Plain, bound).await;
        assert!(prepared.replaced.is_some());
        let Message::Text(text) = prepared.frame else {
            panic!("plain notice required")
        };
        assert_eq!(text.as_str(), serde_json::to_string(&notice).unwrap());
        assert!(prepared.candidate.is_none());
        drop(
            sender
                .try_admit_resync()
                .expect("withheld resync releases admission"),
        );
    }

    #[tokio::test]
    async fn codec_resync_hint_forces_only_its_frame() {
        use crate::socket_mailbox::StateEncoding;
        let raw = raw_state(12);
        let mut encoder = ConnectionEncoder::codec();
        let mut receiver = None;
        deliver(
            &mut encoder,
            &mut receiver,
            envelope(
                projected_update(&raw, PlayerId(0), 100, Vec::new()),
                Some(CommandId(77)),
            ),
            Encoding::Plain,
        )
        .await;
        let mut recovery = envelope(projected_update(&raw, PlayerId(0), 100, Vec::new()), None);
        assert_eq!(
            state_codec::encode(Some(&delivered(&encoder).state), &wire_state(&recovery))
                .unwrap()
                .kind(),
            FrameKind::Delta,
            "equal-revision command removal is delta eligible"
        );
        let (sender, _mailbox) = crate::socket_mailbox::channel();
        recovery.state_encoding = StateEncoding::Resync(sender.try_admit_resync().unwrap());
        let expected = serde_json::to_vec(&recovery).unwrap();
        let before = delivered(&encoder).state.as_bytes().to_vec();
        assert_ne!(
            before, expected,
            "candidate removes the delivered command metadata"
        );
        let prepared = encoder.prepare(recovery, Encoding::Plain).unwrap();
        let next_permit = sender
            .try_admit_resync()
            .expect("encoding releases before flush");
        drop(next_permit);
        assert_eq!(
            delivered(&encoder).state.as_bytes(),
            before,
            "preparation is not commit"
        );
        let frame = codec_frame(prepared.frame).await;
        assert_eq!(frame.kind(), FrameKind::Full);
        let decoded = state_codec::decode(None, &frame).unwrap();
        assert_eq!(decoded.as_bytes(), expected);
        encoder.commit(prepared.candidate);
        receiver = Some(decoded);
        assert_eq!(
            deliver(
                &mut encoder,
                &mut receiver,
                envelope(projected_update(&raw, PlayerId(0), 101, Vec::new()), None),
                Encoding::Plain
            )
            .await,
            FrameKind::Delta,
            "hint does not leak to ordinary frame"
        );
    }

    #[tokio::test]
    async fn codec_resync_preparation_failure_releases_ownership() {
        let raw = raw_state(12);
        let (sender, _mailbox) = crate::socket_mailbox::channel();
        let mut invalid = projected_update(&raw, PlayerId(0), 101, Vec::new());
        if let ServerMessage::StateUpdate { state, .. } = &mut invalid {
            state.viewer_projection = None;
        }
        let mut invalid = envelope(invalid, None);
        invalid.state_encoding =
            crate::socket_mailbox::StateEncoding::Resync(sender.try_admit_resync().unwrap());
        let outcome = encode_application(
            &GameExecutor::new(NonZeroUsize::MIN),
            ConnectionEncoder::codec(),
            invalid,
            Encoding::Plain,
            server_core::game_state_snapshot_wire_guard::MAX_SERVER_FRAME_BYTES,
        )
        .await
        .unwrap();
        assert!(matches!(
            outcome.prepared,
            Err(EncodingFailure::Transport(_))
        ));
        let mut valid = envelope(projected_update(&raw, PlayerId(0), 101, Vec::new()), None);
        valid.state_encoding = crate::socket_mailbox::StateEncoding::Resync(
            sender
                .try_admit_resync()
                .expect("fatal prepare released admission"),
        );
        let prepared = outcome.encoder.prepare(valid, Encoding::Plain).unwrap();
        assert!(sender.try_admit_resync().is_ok());
        assert_eq!(codec_frame(prepared.frame).await.kind(), FrameKind::Full);
    }

    #[tokio::test]
    async fn codec_preserves_non_state_messages_and_automatic_full() {
        let raw = raw_state(12);
        let mut encoder = ConnectionEncoder::codec();
        let mut receiver = None;
        deliver(
            &mut encoder,
            &mut receiver,
            envelope(projected_update(&raw, PlayerId(0), 100, Vec::new()), None),
            Encoding::Plain,
        )
        .await;
        let before = delivered(&encoder).state.as_bytes().to_vec();
        for encoding in [Encoding::Pending, Encoding::Plain, Encoding::GzipEnvelope] {
            for message in [
                ServerMessage::error("ordinary error"),
                ServerMessage::Pong { timestamp: 42 },
                ServerMessage::AuthoritativeStateExport {
                    state: serde_json::to_string(&raw).unwrap(),
                },
            ] {
                let expected = ConnectionEncoder::Legacy
                    .prepare(envelope(message.clone(), Some(CommandId(9))), encoding)
                    .unwrap();
                let outcome = encode_application(
                    &GameExecutor::new(NonZeroUsize::MIN),
                    encoder,
                    envelope(message, Some(CommandId(9))),
                    encoding,
                    server_core::game_state_snapshot_wire_guard::MAX_SERVER_FRAME_BYTES,
                )
                .await
                .unwrap();
                encoder = outcome.encoder;
                let prepared = outcome.prepared.unwrap();
                assert_eq!(prepared.frame, expected.frame);
                assert!(prepared.candidate.is_none());
                encoder.commit(prepared.candidate);
                assert_eq!(delivered(&encoder).state.as_bytes(), before);
            }
        }
        assert_eq!(
            deliver(
                &mut encoder,
                &mut receiver,
                envelope(projected_update(&raw, PlayerId(0), 101, Vec::new()), None),
                Encoding::Plain
            )
            .await,
            FrameKind::Delta
        );
        let mut started = projected_started(&raw, PlayerId(0));
        if let ServerMessage::GameStarted { state_revision, .. } = &mut started {
            *state_revision = 101;
        }
        assert_eq!(
            deliver(
                &mut encoder,
                &mut receiver,
                envelope(started, None),
                Encoding::Plain
            )
            .await,
            FrameKind::Full
        );

        // A large mass removal makes a removal patch larger than the resulting
        // small typed snapshot. Selection remains entirely inside state-codec.
        let large = raw_state(4096);
        let mut encoder = ConnectionEncoder::codec();
        let mut receiver = None;
        deliver(
            &mut encoder,
            &mut receiver,
            envelope(projected_update(&large, PlayerId(0), 200, Vec::new()), None),
            Encoding::Plain,
        )
        .await;
        assert_eq!(
            deliver(
                &mut encoder,
                &mut receiver,
                envelope(projected_update(&large, PlayerId(0), 201, Vec::new()), None),
                Encoding::Plain
            )
            .await,
            FrameKind::Delta
        );
        let empty = raw_state(0);
        let candidate = envelope(projected_update(&empty, PlayerId(0), 202, Vec::new()), None);
        assert_eq!(
            state_codec::encode(Some(&delivered(&encoder).state), &wire_state(&candidate))
                .unwrap()
                .kind(),
            FrameKind::Full,
            "size-fallback reach guard"
        );
        assert_eq!(
            deliver(&mut encoder, &mut receiver, candidate, Encoding::Plain).await,
            FrameKind::Full
        );
    }

    #[tokio::test]
    async fn ws_sent_payload_classifies_prepared_frames() {
        for encoding in [Encoding::Plain, Encoding::GzipEnvelope] {
            let raw = raw_state(12);
            let large = raw_state(4096);
            let empty = raw_state(0);
            let sent = Arc::new(ServerMetrics::default());
            let backlog = Arc::new(PendingMetrics::default());
            let death = Death::new(sent.clone(), 0);
            let (application, rx) = mpsc::channel(1);
            let sink = Box::pin(futures_util::sink::unfold((), |(), _| async {
                Ok::<_, axum::Error>(())
            }));
            let writer = tokio::spawn(write_socket(
                sink,
                rx,
                ControlLane::default(),
                death.clone(),
                backlog.clone(),
                Liveness::default(),
                sent.clone(),
            ));
            let mut totals = [0_u64; 6];
            for codec in [false, true] {
                let mut encoder = if codec {
                    ConnectionEncoder::codec()
                } else {
                    ConnectionEncoder::Legacy
                };
                let mut reference_baseline = None;
                let messages = [
                    (
                        projected_started(&raw, PlayerId(0)),
                        WsFrameType::GameStarted,
                        FrameKind::Full,
                        false,
                    ),
                    (
                        projected_update(&raw, PlayerId(0), 101, vec![]),
                        WsFrameType::StateUpdate,
                        FrameKind::Full,
                        false,
                    ),
                    (
                        projected_update(&raw, PlayerId(0), 102, vec![]),
                        WsFrameType::StateUpdate,
                        FrameKind::Delta,
                        false,
                    ),
                    (
                        projected_update(&raw, PlayerId(0), 102, vec![]),
                        WsFrameType::StateUpdate,
                        FrameKind::Full,
                        true,
                    ),
                    (
                        projected_update(&raw, PlayerId(0), 103, vec![]),
                        WsFrameType::StateUpdate,
                        FrameKind::Delta,
                        false,
                    ),
                    (
                        projected_started(&large, PlayerId(0)),
                        WsFrameType::GameStarted,
                        FrameKind::Full,
                        false,
                    ),
                    (
                        projected_update(&large, PlayerId(0), 200, vec![]),
                        WsFrameType::StateUpdate,
                        FrameKind::Full,
                        false,
                    ),
                    (
                        projected_update(&large, PlayerId(0), 201, vec![]),
                        WsFrameType::StateUpdate,
                        FrameKind::Delta,
                        false,
                    ),
                    (
                        projected_update(&empty, PlayerId(0), 202, vec![]),
                        WsFrameType::StateUpdate,
                        FrameKind::Full,
                        false,
                    ),
                    (
                        ServerMessage::error("ordinary error"),
                        WsFrameType::Other,
                        FrameKind::Full,
                        false,
                    ),
                    (
                        ServerMessage::Pong { timestamp: 42 },
                        WsFrameType::Other,
                        FrameKind::Full,
                        false,
                    ),
                    (
                        ServerMessage::ActionFailed {
                            message: "quota refusal".into(),
                        },
                        WsFrameType::Other,
                        FrameKind::Full,
                        false,
                    ),
                    (
                        ServerMessage::AuthoritativeStateExport {
                            state: serde_json::to_string(&raw).unwrap(),
                        },
                        WsFrameType::Other,
                        FrameKind::Full,
                        false,
                    ),
                ];
                for (message, legacy_class, kind, resync) in messages {
                    let is_started = matches!(&message, ServerMessage::GameStarted { .. });
                    let is_state = matches!(
                        &message,
                        ServerMessage::GameStarted { .. } | ServerMessage::StateUpdate { .. }
                    );
                    let mut envelope = envelope(message, None);
                    let serialized = serde_json::to_string(&envelope).unwrap();
                    let (reference_json, class) = if codec && is_state {
                        let state = wire_state(&envelope);
                        let base = if is_started || resync {
                            None
                        } else {
                            reference_baseline.as_ref()
                        };
                        let reference = state_codec::encode(base, &state).unwrap();
                        assert_eq!(reference.kind(), kind, "codec-alone exact pair premise");
                        let class = match kind {
                            FrameKind::Full => WsFrameType::Full,
                            FrameKind::Delta => WsFrameType::Delta,
                        };
                        reference_baseline = Some(state);
                        (
                            String::from_utf8(reference.to_bytes().unwrap()).unwrap(),
                            class,
                        )
                    } else {
                        (serialized, legacy_class)
                    };
                    let reference = wire::encode_json_message_sync(
                        reference_json,
                        encoding == Encoding::GzipEnvelope,
                    )
                    .unwrap();
                    let expected_bytes = match &reference {
                        Message::Text(text) => text.len(),
                        Message::Binary(bytes) => {
                            if is_state {
                                assert_eq!(bytes[0], 1);
                            }
                            bytes.len()
                        }
                        other => panic!("application reference: {other:?}"),
                    };
                    assert!(expected_bytes > 0);
                    let (sender, _mailbox) = crate::socket_mailbox::channel();
                    if resync {
                        envelope.state_encoding = crate::socket_mailbox::StateEncoding::Resync(
                            sender.try_admit_resync().unwrap(),
                        );
                    }
                    let prepared = encoder.prepare(envelope, encoding).unwrap();
                    assert_eq!(
                        prepared.frame, reference,
                        "same-Envelope independent framing"
                    );
                    let (ack, completion) = oneshot::channel();
                    application
                        .send(PendingWrite::new(
                            prepared.frame,
                            prepared.frame_type,
                            Some(ack),
                            &backlog,
                        ))
                        .await
                        .unwrap();
                    completion.await.unwrap().unwrap();
                    encoder.commit(prepared.candidate);
                    totals[class.index()] += expected_bytes as u64;
                    assert_eq!(
                        WsFrameType::ALL.map(|kind| sent.ws_sent_payload_bytes(kind)),
                        totals
                    );
                    println!("P4-3 {encoding:?} codec={codec} resync={resync} class={class:?} payload={expected_bytes}");
                }
            }
            death.record(Cause::ApplicationEnd);
            writer.await.unwrap();
            assert_eq!(backlog.payload_bytes.load(Ordering::Relaxed), 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::socket_control::RESPONSE_TIMEOUT;

    /// Distinguish accepting a frame from completing its flush.
    struct FlushSink {
        started: mpsc::UnboundedSender<Message>,
        reached: Option<oneshot::Sender<()>>,
        release: Option<oneshot::Receiver<bool>>,
        ready_error: bool,
    }

    impl Sink<Message> for FlushSink {
        type Error = axum::Error;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            if self.ready_error {
                return std::task::Poll::Ready(Err(axum::Error::new(std::io::Error::other(
                    "fixture closed sink",
                ))));
            }
            std::task::Poll::Ready(Ok(()))
        }

        fn start_send(self: std::pin::Pin<&mut Self>, frame: Message) -> Result<(), Self::Error> {
            self.started.send(frame).unwrap();
            Ok(())
        }

        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            if let Some(reached) = self.reached.take() {
                reached.send(()).unwrap();
            }
            if let Some(release) = &mut self.release {
                use std::future::Future;
                let success = std::task::ready!(std::pin::Pin::new(release).poll(cx)).unwrap();
                self.release = None;
                if !success {
                    return std::task::Poll::Ready(Err(axum::Error::new(std::io::Error::other(
                        "fixture flush failed",
                    ))));
                }
            }
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            self.poll_flush(cx)
        }
    }

    #[tokio::test]
    async fn ws_sent_payload_control_failed_flush_is_excluded() {
        for success in [true, false] {
            let (started, mut frames) = mpsc::unbounded_channel();
            let (reached, flushing) = oneshot::channel();
            let (release, blocked) = oneshot::channel();
            let sink = FlushSink {
                started,
                reached: Some(reached),
                release: Some(blocked),
                ready_error: false,
            };
            let sent = Arc::new(ServerMetrics::default());
            let backlog = Arc::new(PendingMetrics::default());
            let death = Death::new(sent.clone(), 0);
            let (_application, rx) = mpsc::channel(1);
            let control = ControlLane::default();
            control.native_pong(axum::body::Bytes::from_static(&[19, 77, 5]));
            let writer = tokio::spawn(write_socket(
                sink,
                rx,
                control.clone(),
                death.clone(),
                backlog.clone(),
                Liveness::default(),
                sent.clone(),
            ));
            assert!(
                matches!(frames.recv().await, Some(Message::Pong(bytes)) if bytes.as_ref() == [19, 77, 5])
            );
            flushing.await.unwrap();
            assert_eq!(sent.ws_sent_payload_bytes(WsFrameType::Control), 0);
            release.send(success).unwrap();
            if success {
                control.close(None, CloseOrigin::Server, || {
                    death.record(Cause::ApplicationEnd)
                });
            }
            writer.await.unwrap();
            assert_eq!(
                sent.ws_sent_payload_bytes(WsFrameType::Control),
                if success { 3 } else { 0 }
            );
            assert_eq!(backlog.payload_bytes.load(Ordering::Relaxed), 0);
            println!(
                "P4-8 native Pong start_send=3, poll_flush reached, success={success}, count={}",
                if success { 3 } else { 0 }
            );
        }
    }

    #[tokio::test]
    async fn ws_sent_payload_counts_only_completed_writes() {
        let (started, mut frames) = mpsc::unbounded_channel();
        let (reached, flushing) = oneshot::channel();
        let (release, blocked) = oneshot::channel();
        let sink = FlushSink {
            started,
            reached: Some(reached),
            release: Some(blocked),
            ready_error: false,
        };
        let sent = Arc::new(ServerMetrics::default());
        let backlog = Arc::new(PendingMetrics::default());
        let death = Death::new(sent.clone(), 0);
        let (application, rx) = mpsc::channel(1);
        let writer = tokio::spawn(write_socket(
            sink,
            rx,
            ControlLane::default(),
            death.clone(),
            backlog.clone(),
            Liveness::default(),
            sent.clone(),
        ));
        let (ack, completion) = oneshot::channel();
        application
            .send(PendingWrite::new(
                Message::text("é".repeat(80)),
                WsFrameType::Other,
                Some(ack),
                &backlog,
            ))
            .await
            .unwrap();
        assert!(matches!(frames.recv().await, Some(Message::Text(text)) if text.len() == 160));
        flushing.await.unwrap();
        assert_eq!(sent.ws_sent_payload_bytes(WsFrameType::Other), 0);
        release.send(true).unwrap();
        completion.await.unwrap().unwrap();
        for payload in [vec![7; 257], Vec::new()] {
            let (ack, completion) = oneshot::channel();
            application
                .send(PendingWrite::new(
                    Message::Binary(payload.into()),
                    WsFrameType::Full,
                    Some(ack),
                    &backlog,
                ))
                .await
                .unwrap();
            completion.await.unwrap().unwrap();
        }
        death.record(Cause::ApplicationEnd);
        writer.await.unwrap();
        assert_eq!(sent.ws_sent_payload_bytes(WsFrameType::Other), 160);
        assert_eq!(sent.ws_sent_payload_bytes(WsFrameType::Full), 257);
        assert_eq!(backlog.frames.load(Ordering::Relaxed), 0);
        assert_eq!(backlog.payload_bytes.load(Ordering::Relaxed), 0);
        println!("P4-1: preflush=0; completed UTF-8=160, binary=257, empty=0; backlog=0");
    }

    #[tokio::test(start_paused = true)]
    async fn ws_sent_payload_failed_flush_is_excluded() {
        #[derive(Debug, Clone, Copy)]
        enum Failure {
            Flush,
            Ready,
            Timeout,
        }
        for failure in [Failure::Flush, Failure::Ready, Failure::Timeout] {
            let (started, mut frames) = mpsc::unbounded_channel();
            let (reached, flushing) = oneshot::channel();
            let (release, blocked) = oneshot::channel();
            let sink = FlushSink {
                started,
                reached: Some(reached),
                release: Some(blocked),
                ready_error: matches!(failure, Failure::Ready),
            };
            let sent = Arc::new(ServerMetrics::default());
            let backlog = Arc::new(PendingMetrics::default());
            let death = Death::new(sent.clone(), 0);
            let (application, rx) = mpsc::channel(1);
            let writer = tokio::spawn(write_socket(
                sink,
                rx,
                ControlLane::default(),
                death.clone(),
                backlog.clone(),
                Liveness::default(),
                sent.clone(),
            ));
            let (ack, completion) = oneshot::channel();
            application
                .send(PendingWrite::new(
                    Message::text("é".repeat(80)),
                    WsFrameType::Other,
                    Some(ack),
                    &backlog,
                ))
                .await
                .unwrap();
            if !matches!(failure, Failure::Ready) {
                assert!(
                    matches!(frames.recv().await, Some(Message::Text(text)) if text.len() == 160)
                );
                flushing.await.unwrap();
                application
                    .send(PendingWrite::new(
                        Message::Binary(vec![9; 257].into()),
                        WsFrameType::Full,
                        None,
                        &backlog,
                    ))
                    .await
                    .unwrap();
                assert_eq!(sent.ws_sent_payload_bytes(WsFrameType::Other), 0);
            }
            match failure {
                Failure::Flush => release.send(false).unwrap(),
                Failure::Ready => release.send(true).unwrap(),
                Failure::Timeout => {
                    death.record(Cause::ReadEof);
                    // Retain the pending flush until the shutdown deadline.
                    completion.await.unwrap().unwrap_err();
                    writer.await.unwrap();
                    drop(release);
                    assert_eq!(sent.ws_sent_payload_bytes(WsFrameType::Other), 0);
                    assert_eq!(sent.ws_sent_payload_bytes(WsFrameType::Full), 0);
                    assert_eq!(backlog.payload_bytes.load(Ordering::Relaxed), 0);
                    println!(
                        "P4-2 {failure:?}: start_send=160, poll_flush reached, queued=257, count=0"
                    );
                    continue;
                }
            }
            completion.await.unwrap().unwrap_err();
            writer.await.unwrap();
            assert_eq!(sent.ws_sent_payload_bytes(WsFrameType::Other), 0);
            assert_eq!(sent.ws_sent_payload_bytes(WsFrameType::Full), 0);
            assert_eq!(backlog.frames.load(Ordering::Relaxed), 0);
            assert_eq!(backlog.payload_bytes.load(Ordering::Relaxed), 0);
            if matches!(failure, Failure::Ready) {
                assert!(
                    frames.try_recv().is_err(),
                    "closed sink never accepts a frame"
                );
            }
            println!("P4-2 {failure:?}: completed=Err, sent=0, backlog=0");
        }
    }

    #[tokio::test]
    async fn ws_sent_payload_ack_lifetime_boundary_receipt() {
        for complete in [false, true] {
            let (started, mut frames) = mpsc::unbounded_channel();
            let (reached, flushing) = oneshot::channel();
            let (release, blocked) = oneshot::channel();
            let sink = FlushSink {
                started,
                reached: Some(reached),
                release: Some(blocked),
                ready_error: false,
            };
            let (application, application_rx) = mpsc::channel(1);
            let metrics = Arc::new(PendingMetrics::default());
            let sent = Arc::new(ServerMetrics::default());
            let death = Death::new(sent.clone(), 0);
            let writer = tokio::spawn(write_socket(
                sink,
                application_rx,
                ControlLane::default(),
                death.clone(),
                metrics.clone(),
                Liveness::default(),
                sent.clone(),
            ));
            let (ack, completion) = oneshot::channel();
            application
                .send(PendingWrite::new(
                    Message::text("é".repeat(80)),
                    WsFrameType::Other,
                    Some(ack),
                    &metrics,
                ))
                .await
                .unwrap();
            assert!(matches!(frames.recv().await, Some(Message::Text(text)) if text.len() == 160));
            flushing.await.unwrap();
            assert_eq!(metrics.payload_bytes.load(Ordering::Relaxed), 160);
            assert_eq!(sent.ws_sent_payload_bytes(WsFrameType::Other), 0);
            drop(completion);
            if complete {
                release.send(true).unwrap();
                // A later acknowledged application frame fences the first
                // successful flush even though its notification was abandoned.
                let (ack, completion) = oneshot::channel();
                application
                    .send(PendingWrite::new(
                        Message::text("fence"),
                        WsFrameType::Other,
                        Some(ack),
                        &metrics,
                    ))
                    .await
                    .unwrap();
                completion.await.unwrap().unwrap();
                death.record(Cause::ApplicationEnd);
                writer.await.unwrap();
                println!("G1 complete=true: start_send=160, poll_flush reached, dropped receiver, later ack=Ok");
            } else {
                writer.abort();
                assert!(writer.await.unwrap_err().is_cancelled());
                drop(release);
                println!("G1 complete=false: start_send=160, poll_flush reached, aborted/joined before success");
            }
            drop(application);
            assert_eq!(metrics.frames.load(Ordering::Relaxed), 0);
            assert_eq!(metrics.payload_bytes.load(Ordering::Relaxed), 0);
            assert_eq!(
                sent.ws_sent_payload_bytes(WsFrameType::Other),
                if complete { 165 } else { 0 }
            );
        }
    }

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
        let sent = Arc::new(ServerMetrics::default());
        let death = Death::new(sent.clone(), 0);
        let metrics = Arc::new(PendingMetrics::default());
        let writer = tokio::spawn(write_socket(
            sink,
            application_rx,
            control.clone(),
            death.clone(),
            metrics.clone(),
            Liveness::default(),
            sent.clone(),
        ));
        application
            .send(PendingWrite::new(
                Message::text("first"),
                WsFrameType::Other,
                None,
                &metrics,
            ))
            .await
            .unwrap();
        writing.await.unwrap();
        application
            .send(PendingWrite::new(
                Message::text("second"),
                WsFrameType::Other,
                None,
                &metrics,
            ))
            .await
            .unwrap();
        control.native_pong(axum::body::Bytes::from_static(&[1; 125]));
        control.application_pong(1, None);
        control.application_pong(42, Some(WireFormat::GzipEnvelopeV1));
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
        let expected_heartbeat = wire::encode_json_message_sync(
            serde_json::to_string(&ServerMessage::Pong { timestamp: 42 }).unwrap(),
            true,
        )
        .unwrap();
        let heartbeat_bytes = match &expected_heartbeat {
            Message::Binary(bytes) => {
                assert_eq!(bytes[0], 0, "small heartbeat includes raw prefix");
                bytes.len()
            }
            other => panic!("raw heartbeat reference: {other:?}"),
        };
        assert_eq!(writes.recv().await.unwrap(), expected_heartbeat);
        assert!(matches!(writes.recv().await, Some(Message::Text(text)) if text == "second"));
        death.record(Cause::ApplicationEnd);
        writer.await.unwrap();
        assert_eq!(sent.ws_sent_payload_bytes(WsFrameType::Other), 11);
        assert_eq!(
            sent.ws_sent_payload_bytes(WsFrameType::Control),
            1 + heartbeat_bytes as u64
        );
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
            Arc::default(),
        ));
        application
            .send(PendingWrite::new(
                Message::text("blocked"),
                WsFrameType::Other,
                None,
                &metrics,
            ))
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
        let sent = Arc::new(ServerMetrics::default());
        let death = Death::new(sent.clone(), 0);
        let writer = tokio::spawn(write_socket(
            sink,
            application_rx,
            control.clone(),
            death.clone(),
            Arc::default(),
            Liveness::default(),
            sent.clone(),
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
        assert_eq!(
            sent.ws_sent_payload_bytes(WsFrameType::Control),
            0,
            "empty Ping/Pong payloads are zero bytes"
        );
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
            Arc::default(),
        ));
        application
            .send(PendingWrite::new(
                Message::text("blocked"),
                WsFrameType::Other,
                None,
                &metrics,
            ))
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
            counters.clone(),
        ));
        assert!(matches!(writes.recv().await, Some(Message::Ping(_))));
        let first_flush = tokio::time::Instant::now();
        let mut next_application = application_interval.map(|interval| first_flush + interval);
        for _ in 1..RESPONSE_TIMEOUT.as_secs() {
            tokio::time::advance(Duration::from_secs(1)).await;
            if next_application.is_some_and(|next| next <= tokio::time::Instant::now()) {
                application
                    .send(PendingWrite::new(
                        Message::text("state"),
                        WsFrameType::Other,
                        None,
                        &metrics,
                    ))
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
            Arc::default(),
        ));
        application
            .send(PendingWrite::new(
                Message::text("slow state frame"),
                WsFrameType::Other,
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
    async fn ws_sent_payload_native_control_boundary_receipt() {
        use axum::extract::{ws::WebSocketUpgrade, State};
        use axum::routing::get;
        use std::num::NonZeroUsize;

        type Handoff = mpsc::UnboundedSender<(
            NegotiatedSocket,
            mpsc::Receiver<Received<Message>>,
            SocketTasks,
            Arc<ServerMetrics>,
        )>;
        async fn upgrade(
            ws: WebSocketUpgrade,
            State(handoff): State<Handoff>,
        ) -> axum::response::Response {
            ws.on_upgrade(move |socket| async move {
                let (replies, messages) = crate::socket_mailbox::channel();
                let sent = Arc::new(ServerMetrics::default());
                let (facade, incoming, tasks) = start(
                    socket,
                    messages,
                    SocketConfig {
                        replies,
                        metrics: sent.clone(),
                        executor: crate::game_executor::GameExecutor::new(NonZeroUsize::MIN),
                        max_frame_bytes:
                            server_core::game_state_snapshot_wire_guard::MAX_SERVER_FRAME_BYTES,
                        hooks: crate::socket_test_hooks::Hooks::default(),
                    },
                );
                assert!(handoff.send((facade, incoming, tasks, sent)).is_ok());
            })
        }
        let (handoff, mut accepted) = mpsc::unbounded_channel();
        let app = axum::Router::new()
            .route("/ws", get(upgrade))
            .with_state(handoff);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            for peer_close in [true, false] {
                let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws")).await.unwrap();
                let (facade, _incoming, tasks, sent) = accepted.recv().await.unwrap();
                socket.send(WsMessage::Ping(vec![19, 77, 5].into())).await.unwrap();
                let mut pong_lengths = Vec::new();
                match socket.next().await.unwrap().unwrap() {
                    WsMessage::Pong(bytes) => {
                        assert_eq!(bytes.as_ref(), [19, 77, 5]);
                        pong_lengths.push(bytes.len());
                    }
                    other => panic!("native Ping response: {other:?}"),
                }
                // Explicit application-control response followed by ordinary
                // FIFO traffic establishes reader/writer progress without sleep.
                facade.enqueue_pong(123);
                facade.enqueue_reply(ServerMessage::error("fence")).unwrap();
                let mut heartbeat_bytes = None;
                loop {
                    match socket.next().await.unwrap().unwrap() {
                        WsMessage::Pong(bytes) => pong_lengths.push(bytes.len()),
                        WsMessage::Text(text) => {
                            match serde_json::from_str::<ServerMessage>(&text).unwrap() {
                                ServerMessage::Pong { timestamp: 123 } => heartbeat_bytes = Some(text.len()),
                                ServerMessage::Error { message, .. } if message == "fence" => break,
                                other => panic!("control progress: {other:?}"),
                            }
                        }
                        other => panic!("control progress frame: {other:?}"),
                    }
                }
                assert!(heartbeat_bytes.unwrap() > 0);
                let reason = "terminé";
                if peer_close {
                    socket.send(WsMessage::Close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
                        code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
                        reason: reason.into(),
                    }))).await.unwrap();
                } else {
                    facade.control.close(Some(CloseFrame { code: 1000, reason: reason.into() }),
                        CloseOrigin::Server, || facade.death.record(Cause::ApplicationEnd));
                }
                let mut close_lengths = Vec::new();
                while let Some(frame) = socket.next().await {
                    match frame {
                        Ok(WsMessage::Pong(bytes)) => pong_lengths.push(bytes.len()),
                        Ok(WsMessage::Close(Some(close))) => {
                            assert_eq!(close.reason.as_str(), reason);
                            close_lengths.push(2 + close.reason.len());
                            let _ = socket.flush().await;
                        }
                        Err(_) => break,
                        other => panic!("teardown frame: {other:?}"),
                    }
                }
                tasks.shutdown().await;
                assert_eq!(close_lengths, [10]);
                assert_eq!(sent.ws_sent_payload_bytes(WsFrameType::Control), 3 + heartbeat_bytes.unwrap() as u64 + 10);
                println!("G0 completed writer Control=53; native wire Pong bytes={} (automatic replies excluded)", pong_lengths.iter().sum::<usize>());
                println!("G0 peer_close={peer_close}: native Pong payloads={pong_lengths:?}, application heartbeat={}, Close payloads={close_lengths:?}; automatic Pong excluded; FinishPeerClose unchanged", heartbeat_bytes.unwrap());
            }
        }).await;
        server.abort();
        result.expect("native-control boundary did not complete");
    }

    // Concurrent selected codec traffic must not alter the legacy SAME-Envelope
    // byte oracle. Both lanes exercise plain/gzip and completion removal; the
    // codec sibling independently reconstructs Full, Full, Delta per batch.
    #[tokio::test]
    async fn live_legacy_encoder_preserves_envelope_bytes() {
        use super::codec_tests::{envelope, projected_started, projected_update, raw_state};
        use axum::extract::{ws::WebSocketUpgrade, State};
        use axum::routing::get;
        use server_core::protocol::{CommandId, ServerMode};
        use std::num::NonZeroUsize;

        type Handoff = mpsc::UnboundedSender<(
            NegotiatedSocket,
            mpsc::Receiver<Received<Message>>,
            SocketTasks,
            Arc<ServerMetrics>,
        )>;
        async fn upgrade(
            ws: WebSocketUpgrade,
            State(handoff): State<Handoff>,
        ) -> axum::response::Response {
            ws.on_upgrade(move |socket| async move {
                let (replies, messages) = crate::socket_mailbox::channel();
                let sent = Arc::new(ServerMetrics::default());
                let started = start(
                    socket,
                    messages,
                    SocketConfig {
                        replies,
                        metrics: sent.clone(),
                        executor: crate::game_executor::GameExecutor::new(NonZeroUsize::MIN),
                        max_frame_bytes:
                            server_core::game_state_snapshot_wire_guard::MAX_SERVER_FRAME_BYTES,
                        hooks: crate::socket_test_hooks::Hooks::default(),
                    },
                );
                let (facade, incoming, tasks) = started;
                assert!(handoff.send((facade, incoming, tasks, sent)).is_ok());
            })
        }

        let (handoff, mut accepted) = mpsc::unbounded_channel();
        let app = axum::Router::new()
            .route("/ws", get(upgrade))
            .with_state(handoff);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
                .await
                .unwrap();
            let (mut facade, _incoming, tasks, legacy_sent) = accepted.recv().await.unwrap();
            let raw = raw_state(12);
            let (mut codec_socket, _) =
                tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
                    .await
                    .unwrap();
            let (mut codec_facade, _codec_incoming, codec_tasks, codec_sent) =
                accepted.recv().await.unwrap();
            codec_facade.set_state_codec(Some(StateCodec::StateCodecV1));
            let mut codec_baseline = None;
            let mut legacy_totals = [0_u64; 6];
            let mut codec_totals = [0_u64; 6];
            fn payload_bytes(frame: &Message) -> u64 {
                match frame {
                    Message::Text(text) => text.len() as u64,
                    Message::Binary(bytes) => bytes.len() as u64,
                    other => panic!("application reference: {other:?}"),
                }
            }
            for encoding in [Encoding::Pending, Encoding::Plain, Encoding::GzipEnvelope] {
                match encoding {
                    Encoding::Pending => {}
                    Encoding::Plain => facade.set_wire_format(None),
                    Encoding::GzipEnvelope => {
                        facade.set_wire_format(Some(WireFormat::GzipEnvelopeV1))
                    }
                }
                codec_facade.set_wire_format(
                    (encoding == Encoding::GzipEnvelope).then_some(WireFormat::GzipEnvelopeV1),
                );
                let mut codec_batch = [
                    envelope(projected_started(&raw, PlayerId(1)), Some(CommandId(46))),
                    envelope(
                        projected_update(&raw, PlayerId(1), 101, Vec::new()),
                        Some(CommandId(47)),
                    ),
                    envelope(projected_update(&raw, PlayerId(1), 101, Vec::new()), None),
                ]
                .into_iter();
                let mut codec_expected = Vec::new();
                let batch = [
                    envelope(projected_started(&raw, PlayerId(0)), Some(CommandId(36))),
                    envelope(
                        projected_update(&raw, PlayerId(0), 101, Vec::new()),
                        Some(CommandId(37)),
                    ),
                    envelope(projected_update(&raw, PlayerId(0), 101, Vec::new()), None),
                    envelope(ServerMessage::Pong { timestamp: 42 }, None),
                    envelope(
                        ServerMessage::ServerHello {
                            server_version: "test".into(),
                            build_commit: "test".into(),
                            protocol_version: 1,
                            mode: ServerMode::Full,
                            lobby_protocol_version: None,
                            public_url: None,
                            wire_formats: vec![WireFormat::GzipEnvelopeV1],
                            heartbeat: None,
                            command_correlation: None,
                            state_codecs: Vec::new(),
                            selected_state_codec: None,
                        },
                        None,
                    ),
                ];
                let mut expected = Vec::new();
                for envelope in batch {
                    let is_hello = matches!(&envelope.message, ServerMessage::ServerHello { .. });
                    // Base formula: serialize the exact Envelope once, then use
                    // the unchanged shared framing authority (not prepare).
                    let reference = wire::encode_json_message_sync(
                        serde_json::to_string(&envelope).unwrap(),
                        encoding == Encoding::GzipEnvelope && !is_hello,
                    )
                    .unwrap();
                    match (&envelope.message, &reference, encoding) {
                        (ServerMessage::ServerHello { .. }, Message::Text(_), _) => {}
                        (ServerMessage::ServerHello { .. }, _, _) => {
                            panic!("hello must remain text")
                        }
                        (
                            ServerMessage::Pong { .. },
                            Message::Binary(bytes),
                            Encoding::GzipEnvelope,
                        ) => assert_eq!(bytes[0], 0, "small raw prefix"),
                        (
                            ServerMessage::GameStarted { .. } | ServerMessage::StateUpdate { .. },
                            Message::Binary(bytes),
                            Encoding::GzipEnvelope,
                        ) => assert_eq!(bytes[0], 1, "large gzip prefix"),
                        (_, Message::Text(_), Encoding::Pending | Encoding::Plain) => {}
                        _ => panic!("unexpected legacy frame shape"),
                    }
                    let class = match &envelope.message {
                        ServerMessage::GameStarted { .. } => WsFrameType::GameStarted,
                        ServerMessage::StateUpdate { .. } => WsFrameType::StateUpdate,
                        _ => WsFrameType::Other,
                    };
                    legacy_totals[class.index()] += payload_bytes(&reference);
                    expected.push(reference);
                    facade
                        .enqueue_completion(envelope.message, envelope.command_id)
                        .unwrap();
                    // Interleave separate projected recipients while both real
                    // writers are live; legacy's oracle is still SAME Envelope.
                    if let Some(envelope) = codec_batch.next() {
                        codec_expected.push(serde_json::to_vec(&envelope).unwrap());
                        codec_facade
                            .enqueue_completion(envelope.message, envelope.command_id)
                            .unwrap();
                    }
                }
                // Fully receive this batch before changing the encoding selection:
                // start samples control.encoding() at encode time, not enqueue time.
                for reference in expected {
                    let actual = loop {
                        match socket.next().await.unwrap().unwrap() {
                            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
                            frame => break frame,
                        }
                    };
                    match (reference, actual) {
                        (Message::Text(expected), WsMessage::Text(actual)) => assert_eq!(
                            actual.as_str(),
                            expected.as_str(),
                            "ordered exact text payload"
                        ),
                        (Message::Binary(expected), WsMessage::Binary(actual)) => assert_eq!(
                            actual.as_ref(),
                            expected.as_ref(),
                            "ordered exact binary payload"
                        ),
                        (expected, actual) => {
                            panic!("legacy framing mismatch: expected {expected:?}, got {actual:?}")
                        }
                    }
                }
                for (index, (bytes, kind)) in codec_expected
                    .into_iter()
                    .zip([
                        state_codec::FrameKind::Full,
                        state_codec::FrameKind::Full,
                        state_codec::FrameKind::Delta,
                    ])
                    .enumerate()
                {
                    let previous_baseline = codec_baseline.clone();
                    let (json, received_bytes) = loop {
                        match codec_socket.next().await.unwrap().unwrap() {
                            WsMessage::Text(text) => {
                                assert_ne!(encoding, Encoding::GzipEnvelope);
                                let received_bytes = text.len() as u64;
                                break (text.to_string(), received_bytes);
                            }
                            WsMessage::Binary(bytes) => {
                                assert_eq!(encoding, Encoding::GzipEnvelope);
                                let received_bytes = bytes.len() as u64;
                                break (
                                    wire::decode_client_envelope(bytes.to_vec(), 16 * 1024 * 1024)
                                        .await
                                        .unwrap(),
                                    received_bytes,
                                );
                            }
                            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
                            frame => panic!("live codec sibling: {frame:?}"),
                        }
                    };
                    let frame = state_codec::Frame::from_bytes(json.as_bytes()).unwrap();
                    assert_eq!(
                        frame.kind(),
                        kind,
                        "concurrent codec selection/metadata removal"
                    );
                    let recovered = state_codec::decode(codec_baseline.as_ref(), &frame).unwrap();
                    assert_eq!(recovered.as_bytes(), bytes, "complete exact codec Envelope");
                    codec_baseline = Some(recovered);
                    let current = WireState::from_json(&bytes)
                        .unwrap()
                        .with_revision(if index == 0 { 100 } else { 101 });
                    let reference_frame = state_codec::encode(
                        if index == 0 {
                            None
                        } else {
                            previous_baseline.as_ref()
                        },
                        &current,
                    )
                    .unwrap();
                    assert_eq!(reference_frame.kind(), kind, "codec-alone exact input pair");
                    let reference = wire::encode_json_message_sync(
                        String::from_utf8(reference_frame.to_bytes().unwrap()).unwrap(),
                        encoding == Encoding::GzipEnvelope,
                    )
                    .unwrap();
                    assert_eq!(payload_bytes(&reference), received_bytes);
                    let class = match kind {
                        state_codec::FrameKind::Full => WsFrameType::Full,
                        state_codec::FrameKind::Delta => WsFrameType::Delta,
                    };
                    codec_totals[class.index()] += received_bytes;
                }
            }
            // A later application FIFO frame, not a control Pong, fences each
            // measured stream before counters are observed.
            for (facade, client, totals) in [
                (&facade, &mut socket, &mut legacy_totals),
                (&codec_facade, &mut codec_socket, &mut codec_totals),
            ] {
                let fence = envelope(ServerMessage::Pong { timestamp: 100 }, None);
                let reference =
                    wire::encode_json_message_sync(serde_json::to_string(&fence).unwrap(), true)
                        .unwrap();
                totals[WsFrameType::Other.index()] += payload_bytes(&reference);
                facade
                    .enqueue_completion(fence.message, fence.command_id)
                    .unwrap();
                let actual = loop {
                    match client.next().await.unwrap().unwrap() {
                        WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
                        frame => break frame,
                    }
                };
                match (reference, actual) {
                    (Message::Binary(expected), WsMessage::Binary(actual)) => {
                        assert_eq!(actual, expected)
                    }
                    other => panic!("FIFO fence framing: {other:?}"),
                }
            }
            socket.close(None).await.unwrap();
            tasks.shutdown().await;
            codec_socket.close(None).await.unwrap();
            codec_tasks.shutdown().await;
            assert_eq!(
                WsFrameType::ALL.map(|kind| legacy_sent.ws_sent_payload_bytes(kind)),
                legacy_totals
            );
            assert_eq!(
                WsFrameType::ALL.map(|kind| codec_sent.ws_sent_payload_bytes(kind)),
                codec_totals
            );
            println!(
                "P4-4 SAME-Envelope live totals: legacy={legacy_totals:?}, codec={codec_totals:?}"
            );
            // Exercise fatal provenance on the actual selected start/encoder
            // path, with a successful projected sibling before EACH fault.
            #[derive(Clone, Copy)]
            enum MissingProvenance {
                FullSessionKey,
                ViewerProjection,
            }
            for valid in [
                projected_started(&raw, PlayerId(0)),
                projected_update(&raw, PlayerId(0), 101, Vec::new()),
            ] {
                for missing in [
                    MissingProvenance::FullSessionKey,
                    MissingProvenance::ViewerProjection,
                ] {
                    let (mut socket, _) =
                        tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
                            .await
                            .unwrap();
                    let (facade, _incoming, tasks, _sent) = accepted.recv().await.unwrap();
                    facade.set_state_codec(Some(StateCodec::StateCodecV1));
                    facade.enqueue_reply(valid.clone()).unwrap();
                    let json = loop {
                        match socket.next().await.unwrap().unwrap() {
                            WsMessage::Text(text) => break text,
                            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
                            other => panic!("expected codec FULL: {other:?}"),
                        }
                    };
                    let frame = state_codec::Frame::from_bytes(json.as_bytes()).unwrap();
                    assert_eq!(frame.kind(), state_codec::FrameKind::Full);
                    let decoded = state_codec::decode(None, &frame).unwrap();
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(decoded.as_bytes()).unwrap(),
                        serde_json::to_value(envelope(valid.clone(), None)).unwrap()
                    );
                    let mut invalid = valid.clone();
                    match &mut invalid {
                        ServerMessage::GameStarted {
                            full_key, state, ..
                        }
                        | ServerMessage::StateUpdate {
                            full_key, state, ..
                        } => match missing {
                            MissingProvenance::FullSessionKey => *full_key = None,
                            MissingProvenance::ViewerProjection => state.viewer_projection = None,
                        },
                        _ => unreachable!("projected sibling"),
                    }
                    let mut closed = tasks.closed();
                    facade.enqueue_reply(invalid).unwrap();
                    closed.wait_for(Option::is_some).await.unwrap();
                    assert_eq!(tasks.disconnect().unwrap().cause, Cause::EncodingFailure);
                    tasks.shutdown().await;
                    while let Some(frame) = socket.next().await {
                        match frame {
                            Ok(WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Close(_))
                            | Err(_) => {}
                            Ok(other) => panic!("fatal codec error silently continued: {other:?}"),
                        }
                    }
                }
            }
        })
        .await;
        server.abort();
        result.expect("live legacy encoding did not complete");
    }

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
    async fn blocked_handler_over_capacity_frames_are_refused_not_closed() {
        use crate::socket_mailbox::RefusalReason;
        let (url, server, _directory, app) = spawn_full_mode_server().await;
        let metrics = app.context.metrics.clone();
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let padded_subscribe = || {
                serde_json::json!({
                    "type": "SubscribeLobby",
                    "padding": "x".repeat(60 * 1024),
                })
                .to_string()
            };
            let correlated = |command_id: u64| {
                let mut frame = serde_json::to_value(ClientMessage::Action {
                    action: engine::types::actions::GameAction::PassPriority,
                })
                .unwrap();
                frame["command_id"] = serde_json::json!(command_id);
                frame["padding"] = serde_json::json!("x".repeat(60 * 1024));
                frame.to_string()
            };
            let retained = MAX_PAYLOAD_BYTES / padded_subscribe().len();
            assert!(MAX_PAYLOAD_BYTES - retained * padded_subscribe().len() < correlated(1).len());

            let mut socket = connect_and_hello(url.clone()).await;
            let held = app.lobby.lock().await;
            // One more than the retained budget holds: it is discarded, not closed.
            for _ in 0..=retained {
                socket
                    .send(WsMessage::Text(padded_subscribe().into()))
                    .await
                    .unwrap();
            }
            for command_id in 41..44 {
                socket
                    .send(WsMessage::Text(correlated(command_id).into()))
                    .await
                    .unwrap();
                loop {
                    match socket.next().await {
                        Some(Ok(WsMessage::Text(text))) => {
                            let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
                            if frame["type"] == "ActionFailed" {
                                assert_eq!(frame["command_id"], command_id);
                                assert_eq!(
                                    frame["data"]["message"],
                                    RefusalReason::GameBusy.message()
                                );
                                break;
                            }
                        }
                        Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => {}
                        other => panic!("an over-capacity frame closed its socket: {other:?}"),
                    }
                }
            }
            send_test_message(&mut socket, &ClientMessage::Ping { timestamp: 7 }, false).await;
            while !matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::Pong { timestamp: 7 }
            ) {}
            assert!(metrics.ws_quota_discard_count() >= 1);
            drop(held);
            while !matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::LobbyUpdate { .. }
            ) {}
            assert_eq!(metrics.ws_disconnect_count(Cause::IngressOverflow), 0);

            // Sustained over-capacity traffic is still bounded by a close.
            let mut hostile = connect_and_hello(url).await;
            let held = app.lobby.lock().await;
            for _ in 0..retained + 2 * crate::socket_decode::MAX_QUOTA_DISCARDS_PER_WINDOW as usize
            {
                if hostile
                    .send(WsMessage::Text(padded_subscribe().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            loop {
                match hostile.next().await {
                    Some(Ok(WsMessage::Close(Some(close)))) => {
                        assert_eq!(u16::from(close.code), 1013);
                        let _ = hostile.close(None).await;
                        break;
                    }
                    Some(Ok(_)) => {}
                    other => panic!("abuse must end in an explicit retryable close: {other:?}"),
                }
            }
            assert_eq!(
                metrics.ws_disconnect_count(Cause::QuotaDiscardOverflow)
                    + metrics.ws_disconnect_count(Cause::IngressOverflow),
                1
            );
            drop(held);
        })
        .await;
        server.abort();
        result.expect("a blocked handler closed or stalled an over-capacity connection");
    }

    #[tokio::test]
    async fn local_capacity_close_drains_peer_ack_with_bounded_bytes() {
        use crate::socket_ingress::{Usage, MAX_REFUSAL_TRANSIT};
        for (text, expected_reads, retained) in [
            (
                r#"{"type":"SubscribeLobby"}"#.to_string(),
                MAX_INGRESS_FRAMES + 2,
                Usage {
                    commands: MAX_COMMANDS,
                    payload_bytes: MAX_COMMANDS * r#"{"type":"SubscribeLobby"}"#.len(),
                    refusal_transit: MAX_REFUSAL_TRANSIT,
                },
            ),
            (
                "x".repeat(crate::MAX_WS_MESSAGE_BYTES),
                2 * MAX_PAYLOAD_BYTES / crate::MAX_WS_MESSAGE_BYTES + 1 + MAX_REFUSAL_TRANSIT,
                Usage {
                    commands: MAX_PAYLOAD_BYTES / crate::MAX_WS_MESSAGE_BYTES,
                    payload_bytes: MAX_PAYLOAD_BYTES,
                    refusal_transit: MAX_REFUSAL_TRANSIT,
                },
            ),
        ] {
            let reads = Arc::new(AtomicUsize::new(0));
            let observed = reads.clone();
            let frames =
                std::iter::repeat_with(|| Ok::<_, axum::Error>(Message::text(text.clone())))
                    .take(MAX_INGRESS_FRAMES + 1)
                    .chain([Ok(Message::Close(None))]);
            let stream = futures_util::stream::iter(frames).inspect(move |_| {
                observed.fetch_add(1, Ordering::SeqCst);
            });
            let control = ControlLane::default();
            let death = Death::new(Arc::default(), 1);
            let budget = IngressBudget::default();
            let (incoming, _commands) = mpsc::channel(MAX_INGRESS_FRAMES);
            read_socket(
                stream,
                control.clone(),
                incoming,
                death.clone(),
                Liveness::default(),
                budget.clone(),
                1,
            )
            .await;
            assert_eq!(death.event().unwrap().cause, Cause::IngressOverflow);
            assert_eq!(budget.usage(), retained);
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
            let (incoming, mut commands) = mpsc::channel(MAX_INGRESS_FRAMES);
            let count = crate::socket_heartbeat::FAST_PATH_BURST as usize + MAX_INGRESS_FRAMES + 1;
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
            assert_eq!(
                budget.usage().refusal_transit,
                crate::socket_ingress::MAX_REFUSAL_TRANSIT
            );
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

/// Test-only production encoder access. CPU and indexed-memory observations use
/// different owners; the collecting sink measures harness wait, not network RTT.
#[cfg(test)]
pub(crate) mod resource_probe {
    use super::*;
    use state_codec::BaselineMemoryUsage;

    pub(crate) struct Owner {
        encoder: ConnectionEncoder,
        application: mpsc::Sender<PendingWrite>,
        received: mpsc::UnboundedReceiver<Message>,
        backlog: Arc<PendingMetrics>,
        death: Death,
        writer: JoinHandle<()>,
    }

    /// Non-Clone transaction: no sender baseline can escape to seed a receiver.
    pub(crate) struct Prepared(PreparedApplication);

    pub(crate) struct Delivery {
        pub(crate) frame: Message,
        pub(crate) queue_flush: Duration,
        pub(crate) commit: Duration,
    }

    impl Owner {
        pub(crate) fn legacy() -> Self {
            Self::new(ConnectionEncoder::Legacy)
        }

        pub(crate) fn codec() -> Self {
            Self::new(ConnectionEncoder::codec())
        }

        fn new(encoder: ConnectionEncoder) -> Self {
            let sent = Arc::new(ServerMetrics::default());
            let backlog = Arc::new(PendingMetrics::default());
            let death = Death::new(sent.clone(), 0);
            let (application, rx) = mpsc::channel(1);
            let (collected, received) = mpsc::unbounded_channel();
            let sink = Box::pin(futures_util::sink::unfold(
                collected,
                |collected, frame| async move {
                    collected.send(frame).unwrap();
                    Ok::<_, axum::Error>(collected)
                },
            ));
            let writer = tokio::spawn(write_socket(
                sink,
                rx,
                ControlLane::default(),
                death.clone(),
                backlog.clone(),
                Liveness::default(),
                sent,
            ));
            Self {
                encoder,
                application,
                received,
                backlog,
                death,
                writer,
            }
        }

        pub(crate) fn prepare(
            &self,
            envelope: crate::socket_mailbox::Envelope,
            encoding: Encoding,
        ) -> (Prepared, Duration) {
            let started = Instant::now();
            let prepared = self.encoder.prepare(envelope, encoding);
            let elapsed = started.elapsed();
            (Prepared(prepared.expect("production preparation")), elapsed)
        }

        /// The estimator initializes lazy caches. Only accounting owners call it.
        pub(crate) fn memory(&self) -> Option<BaselineMemoryUsage> {
            match &self.encoder {
                ConnectionEncoder::Legacy => None,
                ConnectionEncoder::Codec(delivered) => delivered
                    .baseline
                    .as_ref()
                    .map(|baseline| baseline.state.baseline_memory_usage()),
            }
        }

        pub(crate) async fn deliver(&mut self, prepared: Prepared) -> Delivery {
            let PreparedApplication {
                frame,
                frame_type,
                candidate,
                replaced: _,
            } = prepared.0;
            let (ack, completion) = oneshot::channel();
            let pending = PendingWrite::new(frame, frame_type, Some(ack), &self.backlog);
            let started = Instant::now();
            self.application.send(pending).await.unwrap();
            completion
                .await
                .unwrap()
                .expect("real writer flush acknowledgement");
            let queue_flush = started.elapsed();
            let started = Instant::now();
            self.encoder.commit(candidate);
            let commit = started.elapsed();
            let frame = self.received.recv().await.unwrap();
            Delivery {
                frame,
                queue_flush,
                commit,
            }
        }
    }

    impl Drop for Owner {
        fn drop(&mut self) {
            self.death.record(Cause::ApplicationEnd);
            self.writer.abort();
        }
    }

    impl Prepared {
        pub(crate) fn frame_type(&self) -> WsFrameType {
            self.0.frame_type
        }

        pub(crate) fn frame(&self) -> &Message {
            &self.0.frame
        }

        pub(crate) fn memory(&self) -> Option<BaselineMemoryUsage> {
            self.0
                .candidate
                .as_ref()
                .map(|baseline| baseline.state.baseline_memory_usage())
        }
    }
}
