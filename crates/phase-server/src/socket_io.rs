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

/// Server-observed ingress time travels with the payload through both queues.
/// It is never taken from a client-controlled timestamp.
#[derive(Debug)]
pub(crate) struct Received<T> {
    pub(crate) message: T,
    pub(crate) received_at: Instant,
    pub(crate) admission: Admission,
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
}

impl PendingWrite {
    fn new(
        frame: Message,
        acknowledgement: Option<oneshot::Sender<Result<(), String>>>,
        metrics: &Arc<PendingMetrics>,
    ) -> Self {
        Self::from_operation(WriteOperation::Frame(frame), acknowledgement, metrics)
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
    messages: mpsc::UnboundedSender<ServerMessage>,
    control: ControlLane,
    death: Death,
    #[cfg(test)]
    hooks: crate::socket_test_hooks::Hooks,
}

impl NegotiatedSocket {
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
        self.messages
            .send(message)
            .map_err(|_| mpsc::error::SendError(()))
    }

    pub(crate) fn enqueue_pong(&self, timestamp: u64) {
        self.control.application_pong(
            timestamp,
            self.uses_gzip_envelope()
                .then_some(WireFormat::GzipEnvelopeV1),
        );
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

    pub(crate) fn ingress_overflow(&self) {
        self.control.close(
            Some(CloseFrame {
                code: 1013,
                reason: Default::default(),
            }),
            CloseOrigin::Server,
            || self.death.record(Cause::IngressOverflow),
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
    pub(crate) replies: mpsc::UnboundedSender<ServerMessage>,
    pub(crate) metrics: Arc<crate::metrics::ServerMetrics>,
    pub(crate) executor: crate::game_executor::GameExecutor,
    #[cfg(test)]
    pub(crate) hooks: crate::socket_test_hooks::Hooks,
}

pub(crate) fn start(
    socket: WebSocket,
    mut messages: mpsc::UnboundedReceiver<ServerMessage>,
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
                let message = tokio::select! {
                    biased;
                    _ = closed.wait_for(Option::is_some) => break,
                    _ = sample.tick() => {
                        sample_backlog(connection, messages.len(), &metrics, &mut warned);
                        continue;
                    }
                    message = messages.recv() => match message { Some(message) => message, None => break },
                };
                sample_backlog(connection, messages.len() + 1, &metrics, &mut warned);
                #[cfg(test)]
                hooks.before_encode(&message).await;
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
                        _ = sample.tick() => sample_backlog(connection, messages.len() + 1, &metrics, &mut warned),
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
                if application
                    .send(PendingWrite::new(frame, Some(flushed), &metrics))
                    .await
                    .is_err()
                {
                    break;
                }
                // Preserve the old mailbox's one-frame-at-a-time encoding:
                // eagerly serializing an entire queue would duplicate every
                // shared GameState as a full byte buffer during a slow write.
                let result = loop {
                    tokio::select! {
                        biased;
                        _ = closed.wait_for(Option::is_some) => return,
                        _ = sample.tick() => sample_backlog(connection, messages.len(), &metrics, &mut warned),
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
        // A control operation cannot preempt the current RFC 6455 frame.
        let write = async {
            match operation {
                WriteOperation::Frame(frame) => sink.send(frame).await,
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
            let reply = lobby_broker::Broker::stateless_reply(
                &lobby_broker::ConnState::default(),
                &lobby_broker::LobbyClientMessage::Ping { timestamp },
            )
            .expect("an unhosted Ping has a stateless broker reply");
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
                // wireEnvelope.ts keeps <256-byte Pings text/FORMAT_RAW.
                // Exempt established controls before retaining command slots;
                // GZIP frames stay subject to the decoder's raw CPU quota.
                let encoding = control.encoding();
                let ping = match encoding {
                    Encoding::Pending => None,
                    Encoding::Plain | Encoding::GzipEnvelope => {
                        crate::socket_heartbeat::timestamp(&frame, encoding.wire_format())
                    }
                };
                if let Some(timestamp) = ping {
                    control.application_pong(timestamp, encoding.wire_format());
                    control.note_application_activity();
                    continue;
                }
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
    message: ServerMessage,
    envelope: bool,
) -> Result<Message, EncodingFailure> {
    let envelope = envelope && !matches!(&message, ServerMessage::ServerHello { .. });
    let state_bearing = matches!(
        &message,
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
    use crate::issue_4548_full_create_tests::{recv_server_message, spawn_full_mode_server};
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
            for _ in 0..20 {
                socket
                    .send(WsMessage::Text(frame.clone().into()))
                    .await
                    .unwrap();
            }
            loop {
                match socket.next().await {
                    Some(Ok(WsMessage::Close(Some(close)))) => {
                        assert_eq!(u16::from(close.code), 1013);
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
    async fn json_ping_survives_an_exhausted_application_rate_limit() {
        let (url, server, _directory, app) = spawn_full_mode_server().await;
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            let mut socket = connect_and_hello(url).await;
            let held = app.lobby.lock().await;
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
        result.expect("rate limiting silently discarded the heartbeat");
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

    async fn assert_buffered_heartbeats_survive(format: Option<server_core::protocol::WireFormat>) {
        use crate::issue_4548_full_create_tests::{
            recv_enveloped_server_message, send_test_message, test_client_hello,
        };
        let (url, server, _directory, app) = spawn_full_mode_server().await;
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
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
            send_test_message(
                &mut socket,
                &ClientMessage::Ping { timestamp: 0 },
                format.is_some(),
            )
            .await;
            let warmup = if format.is_some() {
                recv_enveloped_server_message(&mut socket).await
            } else {
                recv_server_message(&mut socket).await
            };
            assert!(matches!(warmup, ServerMessage::Pong { timestamp: 0 }));
            let held = app.lobby.lock().await;
            send_test_message(
                &mut socket,
                &ClientMessage::SubscribeLobby,
                format.is_some(),
            )
            .await;
            let mut deck = serde_json::to_value(ClientMessage::CreateGame {
                deck: Default::default(),
            })
            .unwrap();
            deck.as_object_mut().unwrap().insert(
                "padding".to_string(),
                serde_json::Value::String("x".repeat(63 * 1024)),
            );
            let deck = serde_json::to_string(&deck).unwrap();
            if format.is_some() {
                let frame = wire::encode_json_message(deck, true).await.unwrap();
                let Message::Binary(bytes) = frame else {
                    panic!("expected envelope")
                };
                socket.send(WsMessage::Binary(bytes)).await.unwrap();
            } else {
                socket.send(WsMessage::Text(deck.into())).await.unwrap();
            }
            // All these heartbeats can be buffered while a game/server is
            // stalled. They are not pending application commands.
            for timestamp in 1..=128 {
                send_test_message(
                    &mut socket,
                    &ClientMessage::Ping { timestamp },
                    format.is_some(),
                )
                .await;
            }
            loop {
                let reply = if format.is_some() {
                    recv_enveloped_server_message(&mut socket).await
                } else {
                    recv_server_message(&mut socket).await
                };
                if matches!(reply, ServerMessage::Pong { timestamp: 128 }) {
                    break;
                }
            }
            drop(held);
        })
        .await;
        server.abort();
        result.expect("buffered heartbeats caused command admission overflow");
    }

    #[tokio::test]
    async fn buffered_text_heartbeats_do_not_consume_command_admission() {
        assert_buffered_heartbeats_survive(None).await;
    }

    #[tokio::test]
    async fn buffered_raw_envelope_heartbeats_do_not_consume_command_admission() {
        assert_buffered_heartbeats_survive(Some(server_core::protocol::WireFormat::GzipEnvelopeV1))
            .await;
    }

    #[tokio::test]
    async fn malformed_binary_envelopes_consume_the_raw_decode_quota() {
        use crate::issue_4548_full_create_tests::{
            recv_enveloped_server_message, send_test_message, test_client_hello,
        };
        use server_core::protocol::WireFormat;
        let (url, server, _directory, _app) = spawn_full_mode_server().await;
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
            recv_server_message(&mut socket).await;
            send_test_message(
                &mut socket,
                &test_client_hello(vec![WireFormat::GzipEnvelopeV1]),
                false,
            )
            .await;
            for _ in 0..crate::RATE_LIMIT_MESSAGES {
                socket
                    .send(WsMessage::Binary(vec![1, 1, 2, 3].into()))
                    .await
                    .unwrap();
            }
            send_test_message(&mut socket, &ClientMessage::SubscribeLobby, true).await;
            send_test_message(&mut socket, &ClientMessage::Ping { timestamp: 91 }, true).await;
            assert!(matches!(
                recv_enveloped_server_message(&mut socket).await,
                ServerMessage::Pong { timestamp: 91 }
            ));
            assert!(
                tokio::time::timeout(Duration::from_millis(250), async {
                    loop {
                        if matches!(
                            recv_enveloped_server_message(&mut socket).await,
                            ServerMessage::LobbyUpdate { .. }
                        ) {
                            break;
                        }
                    }
                })
                .await
                .is_err(),
                "invalid binary traffic bypassed the quota for the next command"
            );
        })
        .await;
        server.abort();
        result.expect("malformed traffic prevented its control reply");
    }
}
