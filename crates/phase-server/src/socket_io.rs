//! Socket I/O is independent of application handlers and snapshot encoding.
//! Application messages retain their FIFO mailbox; only transport control and
//! application Pong replies may overtake a frame that has not started writing.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use server_core::protocol::{ServerMessage, WireFormat};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::{wire, RateLimiter};

const PING_INTERVAL: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(45);
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
}

#[derive(Clone)]
struct Death(watch::Sender<Option<Instant>>);

impl Death {
    fn record(&self) {
        self.0.send_if_modified(|at| {
            if at.is_some() {
                false
            } else {
                *at = Some(Instant::now());
                true
            }
        });
    }
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

enum WriteLane {
    Application,
    Control,
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

/// The handler-facing writer facade. A direct send still waits for its flush,
/// but neither the socket reader nor priority control delivery waits for it.
#[derive(Clone)]
pub(crate) struct NegotiatedSocket {
    application: mpsc::UnboundedSender<PendingWrite>,
    control: mpsc::UnboundedSender<PendingWrite>,
    envelope: Arc<AtomicBool>,
    metrics: Arc<PendingMetrics>,
}

impl NegotiatedSocket {
    pub(crate) fn set_wire_format(&mut self, format: Option<WireFormat>) {
        let envelope = match format {
            Some(WireFormat::GzipEnvelopeV1) => true,
            Some(WireFormat::Unknown) | None => false,
        };
        self.envelope.store(envelope, Ordering::Release);
    }

    pub(crate) fn uses_gzip_envelope(&self) -> bool {
        self.envelope.load(Ordering::Acquire)
    }

    pub(crate) async fn send(&mut self, frame: Message) -> Result<(), axum::Error> {
        let lane = if matches!(
            &frame,
            Message::Ping(_) | Message::Pong(_) | Message::Close(_)
        ) {
            WriteLane::Control
        } else {
            WriteLane::Application
        };
        self.send_frame(frame, lane).await.map_err(axum::Error::new)
    }

    /// Queue heartbeats without coupling decoder progress to an outbound flush.
    pub(crate) async fn enqueue_pong(&self, timestamp: u64) -> Result<(), String> {
        // Hosted-listing activity is refreshed independently; only the reply
        // projection is stateless here, and the broker still defines its shape.
        let reply = lobby_broker::Broker::stateless_reply(
            &lobby_broker::ConnState::default(),
            &lobby_broker::LobbyClientMessage::Ping { timestamp },
        )
        .expect("an unhosted Ping has a stateless broker reply");
        self.enqueue(&crate::to_server_message(reply), WriteLane::Control)
            .await
    }

    pub(crate) async fn enqueue_reply(&self, message: &ServerMessage) -> Result<(), String> {
        self.enqueue(message, WriteLane::Application).await
    }

    async fn enqueue(&self, message: &ServerMessage, lane: WriteLane) -> Result<(), String> {
        let json = serde_json::to_string(message).map_err(|error| error.to_string())?;
        let frame = wire::encode_json_message(json, self.uses_gzip_envelope()).await?;
        self.queue(lane)
            .send(PendingWrite::new(frame, None, &self.metrics))
            .map_err(|_| "WebSocket writer closed".to_string())
    }

    fn queue(&self, lane: WriteLane) -> &mpsc::UnboundedSender<PendingWrite> {
        match lane {
            WriteLane::Application => &self.application,
            WriteLane::Control => &self.control,
        }
    }

    async fn send_frame(&self, frame: Message, lane: WriteLane) -> Result<(), String> {
        let frame = match frame {
            Message::Text(json) if self.uses_gzip_envelope() => {
                wire::encode_json_message(json.to_string(), true).await?
            }
            other => other,
        };
        let (acknowledgement, completed) = oneshot::channel();
        let pending = PendingWrite::new(frame, Some(acknowledgement), &self.metrics);
        self.queue(lane)
            .send(pending)
            .map_err(|_| "WebSocket writer closed".to_string())?;
        completed
            .await
            .map_err(|_| "WebSocket writer closed".to_string())?
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

    pub(crate) fn closed(&self) -> watch::Receiver<Option<Instant>> {
        self.death.0.subscribe()
    }

    pub(crate) fn disconnected_at(&self) -> Option<Instant> {
        *self.death.0.borrow()
    }

    pub(crate) async fn shutdown(mut self) {
        self.death.record();
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
        self.death.record();
        self.reader.abort();
        self.encoder.abort();
        self.writer.abort();
        if let Some(decoder) = &self.decoder {
            decoder.abort();
        }
    }
}

pub(crate) fn start(
    socket: WebSocket,
    mut messages: mpsc::UnboundedReceiver<ServerMessage>,
) -> (
    NegotiatedSocket,
    mpsc::UnboundedReceiver<Received<Message>>,
    SocketTasks,
) {
    let connection = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
    let (sink, stream) = socket.split();
    let (death_tx, _death_rx) = watch::channel(None);
    let death = Death(death_tx);
    let envelope = Arc::new(AtomicBool::new(false));
    let metrics = Arc::new(PendingMetrics::default());
    let (application, application_rx) = mpsc::unbounded_channel::<PendingWrite>();
    let (control, control_rx) = mpsc::unbounded_channel::<PendingWrite>();
    let (incoming, incoming_rx) = mpsc::unbounded_channel();
    let facade = NegotiatedSocket {
        application: application.clone(),
        control: control.clone(),
        envelope: envelope.clone(),
        metrics: metrics.clone(),
    };

    let reader = tokio::spawn(read_socket(
        stream,
        control,
        incoming,
        death.clone(),
        metrics.clone(),
        connection,
    ));

    let encoder = tokio::spawn({
        let death = death.clone();
        let mut closed = death.0.subscribe();
        let metrics = metrics.clone();
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
                let encoding = encode_application(message, envelope.load(Ordering::Acquire));
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
                        death.record();
                        break;
                    }
                };
                let (flushed, mut completion) = oneshot::channel();
                if application
                    .send(PendingWrite::new(frame, Some(flushed), &metrics))
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
                    death.record();
                    break;
                }
            }
        }
    });

    let writer = tokio::spawn(write_socket(
        sink,
        application_rx,
        control_rx,
        death.clone(),
        metrics,
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
    mut application: mpsc::UnboundedReceiver<PendingWrite>,
    mut control: mpsc::UnboundedReceiver<PendingWrite>,
    death: Death,
    metrics: Arc<PendingMetrics>,
) where
    S: Sink<Message, Error = axum::Error> + Unpin,
{
    let mut closed = death.0.subscribe();
    let mut ping =
        tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown_deadline = None;
    loop {
        let mut pending = tokio::select! {
            biased;
            Some(pending) = control.recv() => pending,
            _ = closed.wait_for(Option::is_some) => break,
            _ = ping.tick() => PendingWrite::new(Message::Ping(Vec::new().into()), None, &metrics),
            Some(pending) = application.recv() => pending,
            else => break,
        };
        let operation = pending
            .operation
            .take()
            .expect("queued operation is consumed once");
        let closing = matches!(
            &operation,
            WriteOperation::Frame(Message::Close(_)) | WriteOperation::FinishPeerClose(_)
        );
        // Priority traffic never preempts a partially written frame. Once the
        // transport is dead, bound teardown independently of application work.
        let write = async {
            match operation {
                WriteOperation::Frame(frame) => sink.send(frame).await,
                // The reader already queued tungstenite's Close echo. Sending
                // another frame is rejected as SendAfterClosing; poll_close
                // flushes that automatic response before releasing the socket.
                WriteOperation::FinishPeerClose(_) => sink.close().await,
            }
        };
        tokio::pin!(write);
        let result = tokio::select! {
            biased;
            result = &mut write => result.map_err(|error| error.to_string()),
            _ = async { let _ = closed.wait_for(Option::is_some).await; } => {
                let deadline = *shutdown_deadline.get_or_insert_with(
                    || tokio::time::Instant::now() + SHUTDOWN_TIMEOUT,
                );
                match tokio::time::timeout_at(deadline, &mut write).await {
                    Ok(result) => result.map_err(|error| error.to_string()),
                    Err(_) => Err("WebSocket transport closed during write".to_string()),
                }
            }
        };
        let failed = result.is_err();
        if let Some(acknowledgement) = pending.acknowledgement.take() {
            let _ = acknowledgement.send(result);
        }
        if failed || closing {
            break;
        }
    }
    death.record();
    // A queued Pong can lose a race with an incoming Close too. Even on that
    // write error, flush the library's automatic Close echo before dropping
    // the stream, without extending the transport's teardown deadline.
    let deadline =
        shutdown_deadline.unwrap_or_else(|| tokio::time::Instant::now() + SHUTDOWN_TIMEOUT);
    let _ = tokio::time::timeout_at(deadline, sink.close()).await;
}

async fn read_socket<S>(
    mut stream: S,
    control: mpsc::UnboundedSender<PendingWrite>,
    incoming: mpsc::UnboundedSender<Received<Message>>,
    death: Death,
    metrics: Arc<PendingMetrics>,
    connection: u64,
) where
    S: Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    let mut closed = death.0.subscribe();
    let mut limiter = RateLimiter::new();
    loop {
        let next = tokio::select! {
            biased;
            _ = closed.wait_for(Option::is_some) => break,
            next = tokio::time::timeout(READ_TIMEOUT, stream.next()) => next,
        };
        match next {
            Ok(Some(Ok(Message::Ping(bytes)))) => {
                // RFC 6455 section 5.5.2: Pong echoes the Ping payload.
                let _ = control.send(PendingWrite::new(Message::Pong(bytes), None, &metrics));
            }
            Ok(Some(Ok(Message::Pong(_)))) => {}
            Ok(Some(Ok(Message::Close(close)))) => {
                let _ = control.send(PendingWrite::from_operation(
                    WriteOperation::FinishPeerClose(close),
                    None,
                    &metrics,
                ));
                break;
            }
            Ok(Some(Ok(frame @ (Message::Text(_) | Message::Binary(_))))) => {
                if limiter.check() {
                    if incoming
                        .send(Received {
                            message: frame,
                            received_at: Instant::now(),
                        })
                        .is_err()
                    {
                        break;
                    }
                } else {
                    debug!(
                        connection,
                        "rate limit exceeded, dropping application message"
                    );
                }
            }
            Ok(Some(Err(error))) => {
                debug!(connection, %error, "WebSocket read failed");
                break;
            }
            Ok(None) => break,
            Err(_) => {
                warn!(connection, "WebSocket read deadline expired");
                let _ = control.send(PendingWrite::new(Message::Close(None), None, &metrics));
                break;
            }
        }
    }
    // Capture the instant here, not after a game/session lock is available.
    death.record();
}

enum EncodingFailure {
    Serialization(serde_json::Error),
    Transport(String),
}

async fn encode_application(
    message: ServerMessage,
    envelope: bool,
) -> Result<Message, EncodingFailure> {
    let json = tokio::task::spawn_blocking(move || serde_json::to_string(&message))
        .await
        .map_err(|error| EncodingFailure::Transport(error.to_string()))?
        .map_err(EncodingFailure::Serialization)?;
    wire::encode_json_message(json, envelope)
        .await
        .map_err(EncodingFailure::Transport)
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

    #[tokio::test(start_paused = true)]
    async fn read_deadline_tracks_transport_activity_not_application_progress() {
        let (frames, mut stream_rx) = mpsc::unbounded_channel();
        let (polled, first_poll) = oneshot::channel();
        let mut polled = Some(polled);
        let stream = futures_util::stream::poll_fn(move |cx| {
            if let Some(polled) = polled.take() {
                let _ = polled.send(());
            }
            stream_rx.poll_recv(cx)
        });
        let (control, mut control_rx) = mpsc::unbounded_channel();
        let (incoming, mut incoming_rx) = mpsc::unbounded_channel();
        let (death_tx, _) = watch::channel(None);
        let death = Death(death_tx);
        let mut closed = death.0.subscribe();
        let reader = tokio::spawn(read_socket(
            stream,
            control,
            incoming,
            death.clone(),
            Arc::default(),
            1,
        ));
        first_poll.await.expect("reader polled the stream");
        tokio::time::advance(Duration::from_secs(40)).await;
        frames.send(Ok(Message::text("application frame"))).unwrap();
        assert!(matches!(incoming_rx.recv().await,
            Some(Received { message: Message::Text(text), .. }) if text == "application frame"));
        tokio::time::advance(Duration::from_secs(44)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(1), closed.wait_for(Option::is_some))
                .await
                .is_err(),
            "recent traffic keeps the transport alive"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        let detected =
            *tokio::time::timeout(Duration::from_millis(1), closed.wait_for(Option::is_some))
                .await
                .expect("45-second read deadline")
                .unwrap();
        assert!(matches!(
            control_rx
                .recv()
                .await
                .and_then(|mut pending| pending.operation.take()),
            Some(WriteOperation::Frame(Message::Close(None)))
        ));
        reader.await.unwrap();
        death.record();
        assert_eq!(
            *death.0.borrow(),
            detected,
            "cleanup cannot restart the grace clock"
        );
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
        let (application, application_rx) = mpsc::unbounded_channel();
        let (control, control_rx) = mpsc::unbounded_channel();
        let (death_tx, _) = watch::channel(None);
        let death = Death(death_tx);
        let metrics = Arc::new(PendingMetrics::default());
        let writer = tokio::spawn(write_socket(
            sink,
            application_rx,
            control_rx,
            death.clone(),
            metrics.clone(),
        ));
        application
            .send(PendingWrite::new(Message::text("first"), None, &metrics))
            .unwrap();
        writing.await.unwrap();
        application
            .send(PendingWrite::new(Message::text("second"), None, &metrics))
            .unwrap();
        control
            .send(PendingWrite::new(
                Message::Pong(vec![42].into()),
                None,
                &metrics,
            ))
            .unwrap();
        assert!(
            matches!(writes.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "control cannot preempt the frame already being written"
        );
        gate.send(()).unwrap();
        assert!(matches!(writes.recv().await, Some(Message::Text(text)) if text == "first"));
        assert!(
            matches!(writes.recv().await, Some(Message::Pong(bytes)) if bytes.as_ref() == &[42])
        );
        assert!(matches!(writes.recv().await, Some(Message::Text(text)) if text == "second"));
        death.record();
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
                futures_util::future::pending::<Result<Option<oneshot::Sender<()>>, axum::Error>>()
                    .await
            },
        ));
        let (application, application_rx) = mpsc::unbounded_channel();
        let (_control, control_rx) = mpsc::unbounded_channel();
        let (death_tx, _) = watch::channel(None);
        let death = Death(death_tx);
        let metrics = Arc::new(PendingMetrics::default());
        let writer = tokio::spawn(write_socket(
            sink,
            application_rx,
            control_rx,
            death.clone(),
            metrics.clone(),
        ));
        application
            .send(PendingWrite::new(Message::text("blocked"), None, &metrics))
            .unwrap();
        writing.await.unwrap();
        death.record();
        tokio::time::timeout(SHUTDOWN_TIMEOUT + Duration::from_millis(1), writer)
            .await
            .expect("dead transport teardown cannot wait for a game handler")
            .unwrap();
    }

    #[tokio::test]
    async fn decoder_progresses_while_replies_wait_for_the_writer() {
        use server_core::protocol::{
            build_commit, ClientMessage, ServerMode, LOBBY_PROTOCOL_VERSION, PROTOCOL_VERSION,
        };

        let (application, mut replies) = mpsc::unbounded_channel();
        let (control, mut priority) = mpsc::unbounded_channel();
        let socket = NegotiatedSocket {
            application,
            control,
            envelope: Arc::new(AtomicBool::new(false)),
            metrics: Arc::default(),
        };
        let (frames, incoming) = mpsc::unbounded_channel();
        let (commands, mut received) = mpsc::unbounded_channel();
        let (hello, _accepted) = oneshot::channel();
        let (_host, host) = watch::channel(None);
        let decoder = tokio::spawn(crate::socket_decode::run(
            incoming,
            socket,
            Arc::new(tokio::sync::Mutex::new(lobby_broker::Broker::new())),
            host,
            ServerMode::Full,
            hello,
            commands,
        ));
        let hello = ClientMessage::ClientHello {
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            build_commit: build_commit().to_string(),
            protocol_version: PROTOCOL_VERSION,
            lobby_protocol_version: Some(LOBBY_PROTOCOL_VERSION),
            wire_formats: Vec::new(),
        };
        for json in [
            serde_json::to_string(&hello).unwrap(),
            "{".to_string(),
            serde_json::to_string(&ClientMessage::Ping { timestamp: 77 }).unwrap(),
            serde_json::to_string(&ClientMessage::SubscribeLobby).unwrap(),
        ] {
            frames
                .send(Received {
                    message: Message::text(json),
                    received_at: Instant::now(),
                })
                .unwrap();
        }
        // Neither outbound queue is polled or acknowledged until an ordinary
        // command has crossed the decoder. Backpressure cannot stall ingress.
        let command = tokio::time::timeout(Duration::from_secs(1), received.recv())
            .await
            .expect("decoder waited for an outbound flush")
            .unwrap();
        assert!(matches!(command.message, ClientMessage::SubscribeLobby));
        let mut reply = replies.try_recv().expect("parse error was retained");
        assert!(matches!(
            reply.operation.take(),
            Some(WriteOperation::Frame(Message::Text(json)))
                if matches!(serde_json::from_str::<ServerMessage>(&json).unwrap(),
                    ServerMessage::Error { .. })
        ));
        let mut pong = priority.try_recv().expect("heartbeat reply was retained");
        assert!(matches!(
            pong.operation.take(),
            Some(WriteOperation::Frame(Message::Text(json)))
                if matches!(serde_json::from_str::<ServerMessage>(&json).unwrap(),
                    ServerMessage::Pong { timestamp: 77 })
        ));
        drop(frames);
        decoder.await.unwrap();
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
        let (_application, application_rx) = mpsc::unbounded_channel();
        let (control, control_rx) = mpsc::unbounded_channel();
        let (death_tx, _) = watch::channel(None);
        let death = Death(death_tx);
        let metrics = Arc::new(PendingMetrics::default());
        let writer = tokio::spawn(write_socket(
            sink,
            application_rx,
            control_rx,
            death.clone(),
            metrics.clone(),
        ));
        let (flushed, flush) = oneshot::channel();
        control
            .send(PendingWrite::new(
                Message::Pong(Vec::new().into()),
                Some(flushed),
                &metrics,
            ))
            .unwrap();
        flush.await.unwrap().unwrap();
        let _ = writes.recv().await.unwrap();
        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(1), writes.recv())
                .await
                .is_err(),
            "server Ping must not run before the ten-second interval"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(1), writes.recv())
                .await
                .expect("server Ping is due after ten seconds"),
            Some(Message::Ping(_))
        ));
        death.record();
        writer.await.unwrap();
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
