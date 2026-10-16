//! The first transport failure owns the reconnect clock and diagnostic cause.

use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::watch;

use crate::metrics::ServerMetrics;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cause {
    PeerClose,
    ReadEof,
    ReadError,
    ReadDeadline,
    WriteFailure,
    EncodingFailure,
    IngressOverflow,
    ApplicationEnd,
    RawDecodeQuota,
    QuotaRefusalOverflow,
}

impl Cause {
    pub(crate) const ALL: [Self; 10] = [
        Self::PeerClose,
        Self::ReadEof,
        Self::ReadError,
        Self::ReadDeadline,
        Self::WriteFailure,
        Self::EncodingFailure,
        Self::IngressOverflow,
        Self::ApplicationEnd,
        Self::RawDecodeQuota,
        Self::QuotaRefusalOverflow,
    ];

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::PeerClose => 0,
            Self::ReadEof => 1,
            Self::ReadError => 2,
            Self::ReadDeadline => 3,
            Self::WriteFailure => 4,
            Self::EncodingFailure => 5,
            Self::IngressOverflow => 6,
            Self::ApplicationEnd => 7,
            Self::RawDecodeQuota => 8,
            Self::QuotaRefusalOverflow => 9,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::PeerClose => "peer_close",
            Self::ReadEof => "read_eof",
            Self::ReadError => "read_error",
            Self::ReadDeadline => "read_deadline",
            Self::WriteFailure => "write_failure",
            Self::EncodingFailure => "encoding_failure",
            Self::IngressOverflow => "ingress_overflow",
            Self::ApplicationEnd => "application_end",
            Self::RawDecodeQuota => "raw_decode_quota",
            Self::QuotaRefusalOverflow => "quota_refusal_overflow",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Disconnect {
    pub(crate) at: Instant,
    pub(crate) cause: Cause,
    pub(crate) peer_close_code: Option<u16>,
    pub(crate) read_error_kind: Option<ErrorKind>,
}

#[derive(Clone)]
pub(crate) struct Death {
    state: watch::Sender<Option<Disconnect>>,
    metrics: Arc<ServerMetrics>,
    connection: u64,
}

impl Death {
    pub(crate) fn new(metrics: Arc<ServerMetrics>, connection: u64) -> Self {
        let (state, _) = watch::channel(None);
        Self {
            state,
            metrics,
            connection,
        }
    }

    pub(crate) fn closed(&self) -> watch::Receiver<Option<Disconnect>> {
        self.state.subscribe()
    }

    pub(crate) fn event(&self) -> Option<Disconnect> {
        *self.state.borrow()
    }

    pub(crate) fn record(&self, cause: Cause) {
        self.record_details(cause, None, None);
    }

    pub(crate) fn peer_close(&self, code: Option<u16>) {
        self.record_details(Cause::PeerClose, code, None);
    }

    pub(crate) fn read_error(&self, error: &axum::Error) {
        let mut source: &(dyn std::error::Error + 'static) = error;
        let kind = loop {
            if let Some(error) = source.downcast_ref::<std::io::Error>() {
                break Some(error.kind());
            }
            match source.source() {
                Some(next) => source = next,
                None => break None,
            }
        };
        self.record_details(Cause::ReadError, None, kind);
    }

    fn record_details(
        &self,
        cause: Cause,
        peer_close_code: Option<u16>,
        read_error_kind: Option<ErrorKind>,
    ) {
        let first = self.state.send_if_modified(|state| {
            if state.is_some() {
                return false;
            }
            self.metrics.record_ws_disconnect(cause);
            *state = Some(Disconnect {
                at: Instant::now(),
                cause,
                peer_close_code,
                read_error_kind,
            });
            true
        });
        if first {
            tracing::info!(
                connection = self.connection,
                cause = cause.label(),
                ?peer_close_code,
                ?read_error_kind,
                "WebSocket transport disconnected"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_cannot_overwrite_the_detected_cause_clock_or_counter() {
        let metrics = Arc::new(ServerMetrics::default());
        let death = Death::new(metrics.clone(), 0);
        death.peer_close(Some(1001));
        let first = death.event().unwrap();
        death.record(Cause::WriteFailure);
        death.record(Cause::ApplicationEnd);
        assert_eq!(death.event(), Some(first));
        assert_eq!(first.cause, Cause::PeerClose);
        assert_eq!(first.peer_close_code, Some(1001));
        assert_eq!(metrics.ws_disconnect_count(Cause::PeerClose), 1);
        assert_eq!(metrics.ws_disconnect_count(Cause::WriteFailure), 0);
        assert_eq!(metrics.ws_disconnect_count(Cause::ApplicationEnd), 0);
    }
}
