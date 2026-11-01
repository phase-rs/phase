//! Constant-space control traffic, separate from the application FIFO.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::CloseFrame;
use parking_lot::Mutex;
use server_core::protocol::{StateCodec, WireFormat};
use tokio::sync::{watch, Notify};
use tokio::time::Instant;

pub(crate) const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug)]
pub(crate) enum CloseOrigin {
    Server,
    Peer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Encoding {
    Pending = 0,
    Plain = 1,
    GzipEnvelope = 2,
}

impl Encoding {
    pub(crate) fn wire_format(self) -> Option<WireFormat> {
        match self {
            Self::Pending | Self::Plain => None,
            Self::GzipEnvelope => Some(WireFormat::GzipEnvelopeV1),
        }
    }
}

#[derive(Debug)]
pub(crate) enum Control {
    Close {
        frame: Option<CloseFrame>,
        origin: CloseOrigin,
    },
    NativePong(Bytes),
    ApplicationPong {
        timestamp: u64,
        format: Option<WireFormat>,
    },
}

enum Pending {
    Open {
        native_pong: Option<Bytes>,
        application_pong: Option<(u64, Option<WireFormat>)>,
    },
    Closing(Option<Control>),
}

impl Default for Pending {
    fn default() -> Self {
        Self::Open {
            native_pong: None,
            application_pong: None,
        }
    }
}

#[derive(Default)]
struct Shared {
    pending: Mutex<Pending>,
    changed: Notify,
    application_activity: Notify,
    encoding: AtomicU8,
    state_codec: Mutex<Option<StateCodec>>,
    heartbeat: Mutex<crate::socket_heartbeat::HeartbeatProbe>,
}

#[derive(Clone, Default)]
pub(crate) struct ControlLane(Arc<Shared>);

impl ControlLane {
    pub(crate) fn accept_wire_format(&self, format: Option<WireFormat>) {
        let encoding = match format {
            Some(WireFormat::GzipEnvelopeV1) => Encoding::GzipEnvelope,
            Some(WireFormat::Unknown) | None => Encoding::Plain,
        };
        self.0.encoding.store(encoding as u8, Ordering::Release);
    }

    pub(crate) fn encoding(&self) -> Encoding {
        match self.0.encoding.load(Ordering::Acquire) {
            0 => Encoding::Pending,
            1 => Encoding::Plain,
            2 => Encoding::GzipEnvelope,
            _ => unreachable!("only typed negotiated encodings are stored"),
        }
    }

    pub(crate) fn accept_state_codec(&self, codec: Option<StateCodec>) {
        let mut selected = self.0.state_codec.lock();
        if selected.is_none() && codec == Some(StateCodec::StateCodecV1) {
            *selected = codec;
        }
    }

    pub(crate) fn state_codec(&self) -> Option<StateCodec> {
        *self.0.state_codec.lock()
    }

    pub(crate) fn probe_heartbeat(&self, frame: &axum::extract::ws::Message) -> Option<u64> {
        self.0
            .heartbeat
            .lock()
            .timestamp(frame, self.encoding().wire_format())
    }

    #[cfg(test)]
    pub(crate) fn observe_probe_work(&self, work: Arc<crate::socket_test_hooks::ParserWork>) {
        self.0.heartbeat.lock().observe(work);
    }

    pub(crate) fn note_application_activity(&self) {
        self.0.application_activity.notify_one();
    }

    pub(crate) async fn application_activity(&self) {
        self.0.application_activity.notified().await;
    }

    pub(crate) fn close(
        &self,
        frame: Option<CloseFrame>,
        origin: CloseOrigin,
        detect: impl FnOnce(),
    ) {
        let mut pending = self.0.pending.lock();
        if matches!(*pending, Pending::Open { .. }) {
            *pending = Pending::Closing(Some(Control::Close { frame, origin }));
            // Publish the close operation before waking observers of transport
            // death; a writer must not exit between detection and enqueue.
            detect();
        }
        drop(pending);
        self.0.changed.notify_one();
    }

    pub(crate) fn native_pong(&self, payload: Bytes) {
        let mut pending = self.0.pending.lock();
        if let Pending::Open { native_pong, .. } = &mut *pending {
            *native_pong = Some(payload);
        }
        drop(pending);
        self.0.changed.notify_one();
    }

    pub(crate) fn application_pong(&self, timestamp: u64, format: Option<WireFormat>) {
        let mut pending = self.0.pending.lock();
        if let Pending::Open {
            application_pong, ..
        } = &mut *pending
        {
            *application_pong = Some((timestamp, format));
        }
        drop(pending);
        self.0.changed.notify_one();
    }

    pub(crate) fn take_ready(&self) -> Option<Control> {
        let mut pending = self.0.pending.lock();
        match &mut *pending {
            Pending::Closing(close) => close.take(),
            Pending::Open {
                native_pong,
                application_pong,
            } => native_pong.take().map(Control::NativePong).or_else(|| {
                application_pong
                    .take()
                    .map(|(timestamp, format)| Control::ApplicationPong { timestamp, format })
            }),
        }
    }

    pub(crate) async fn next(&self) -> Control {
        loop {
            let notified = self.0.changed.notified();
            if let Some(next) = self.take_ready() {
                return next;
            }
            notified.await;
        }
    }

    #[cfg(test)]
    pub(crate) fn pending_count(&self) -> usize {
        let pending = self.0.pending.lock();
        match &*pending {
            Pending::Closing(close) => usize::from(close.is_some()),
            Pending::Open {
                native_pong,
                application_pong,
            } => usize::from(native_pong.is_some()) + usize::from(application_pong.is_some()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ReadState {
    last_read: Option<Instant>,
    response_deadline: Option<Instant>,
    write_started: Option<Instant>,
}

impl ReadState {
    pub(crate) fn deadline(self) -> Option<Instant> {
        if self.write_started.is_some() {
            None
        } else {
            self.response_deadline
        }
    }
}

#[derive(Clone)]
pub(crate) struct Liveness(watch::Sender<ReadState>);

impl Default for Liveness {
    fn default() -> Self {
        let (state, _) = watch::channel(ReadState::default());
        Self(state)
    }
}

impl Liveness {
    pub(crate) fn reader(&self) -> watch::Receiver<ReadState> {
        self.0.subscribe()
    }

    pub(crate) fn received(&self) {
        self.0.send_modify(|state| {
            state.last_read = Some(Instant::now());
            state.response_deadline = None;
        });
    }

    pub(crate) fn write_started(&self) -> Instant {
        let started = Instant::now();
        self.0
            .send_modify(|state| state.write_started = Some(started));
        started
    }

    pub(crate) fn write_finished(&self, ping_started: Option<Instant>) {
        let now = Instant::now();
        self.0.send_modify(|state| {
            if let (Some(started), Some(deadline)) =
                (state.write_started.take(), state.response_deadline.as_mut())
            {
                // Pause the remaining response budget for actual write time;
                // an ordinary flush must not mint a fresh response window.
                *deadline += now.duration_since(started);
            }
            // A response may already have arrived before the writer's flush
            // future is repolled. Do not start a fresh unanswered challenge.
            let unanswered_ping = ping_started
                .is_some_and(|started| state.last_read.is_none_or(|read| read < started));
            if unanswered_ping && state.response_deadline.is_none() {
                state.response_deadline = Some(now + RESPONSE_TIMEOUT);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn control_flood_retains_only_latest_pongs_and_close_wins() {
        let control = ControlLane::default();
        for timestamp in 0..10_000 {
            control.application_pong(timestamp, None);
            control.native_pong(Bytes::copy_from_slice(&timestamp.to_be_bytes()));
        }
        assert_eq!(control.pending_count(), 2);
        assert!(matches!(control.next().await, Control::NativePong(bytes)
            if bytes.as_ref() == 9_999_u64.to_be_bytes()));
        assert!(matches!(
            control.next().await,
            Control::ApplicationPong {
                timestamp: 9_999,
                format: None
            }
        ));
        control.application_pong(10_000, None);
        control.close(
            Some(CloseFrame {
                code: 1013,
                reason: "overloaded".into(),
            }),
            CloseOrigin::Server,
            || {},
        );
        for _ in 0..10_000 {
            control.application_pong(10_001, None);
        }
        assert_eq!(control.pending_count(), 1);
        assert!(
            matches!(control.next().await, Control::Close { frame: Some(frame), origin: CloseOrigin::Server }
            if frame.code == 1013)
        );
        assert_eq!(control.pending_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn no_response_deadline_exists_until_a_server_ping_is_flushed() {
        let liveness = Liveness::default();
        let reader = liveness.reader();
        let started = liveness.write_started();
        tokio::time::advance(RESPONSE_TIMEOUT * 3).await;
        assert!(reader.borrow().deadline().is_none());
        liveness.write_finished(Some(started));
        assert_eq!(
            reader.borrow().deadline(),
            Some(Instant::now() + RESPONSE_TIMEOUT)
        );
        liveness.received();
        assert!(reader.borrow().deadline().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_in_flight_frame_suspends_and_then_extends_the_response_deadline() {
        let liveness = Liveness::default();
        let reader = liveness.reader();
        let ping = liveness.write_started();
        liveness.write_finished(Some(ping));
        liveness.write_started();
        tokio::time::advance(RESPONSE_TIMEOUT * 3).await;
        assert!(reader.borrow().deadline().is_none());
        liveness.write_finished(None);
        assert_eq!(
            reader.borrow().deadline(),
            Some(Instant::now() + RESPONSE_TIMEOUT)
        );
        liveness.received();
        assert!(reader.borrow().deadline().is_none());
    }
}
