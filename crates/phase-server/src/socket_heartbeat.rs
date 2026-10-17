//! Recognize application heartbeats before retaining application commands.
//!
//! client/src/network/wireEnvelope.ts uses a 256-byte compression threshold;
//! its ~40-byte Ping is always plain text or FORMAT_RAW. Compressed envelopes
//! deliberately remain subject to raw decode admission, regardless of content.

use axum::extract::ws::Message;
use serde::Deserialize;
use server_core::protocol::WireFormat;

#[derive(Deserialize)]
enum MessageKind {
    Ping,
    #[serde(other)]
    Other,
}

#[derive(Default, Deserialize)]
struct PingData {
    #[serde(default)]
    timestamp: Option<u64>,
}

#[derive(Deserialize)]
struct PingProbe {
    #[serde(rename = "type")]
    kind: MessageKind,
    #[serde(default)]
    data: PingData,
}

/// Match the client's wireEnvelope.ts compression threshold. Never parse a
/// command-sized frame on the reader before ingress admission.
pub(crate) const MAX_HEARTBEAT_PROBE_BYTES: usize = 256;
pub(crate) const FAST_PATH_BURST: u32 = 8;
const PROBE_REFILL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Bound fast-path heartbeats and malformed probes. Valid application commands
/// preserve the heartbeat reserve and still require ingress/decoder admission.
pub(crate) struct HeartbeatProbe {
    tokens: u32,
    refilled_at: tokio::time::Instant,
}

impl Default for HeartbeatProbe {
    fn default() -> Self {
        Self {
            tokens: FAST_PATH_BURST,
            refilled_at: tokio::time::Instant::now(),
        }
    }
}

impl HeartbeatProbe {
    pub(crate) fn timestamp(&mut self, frame: &Message, format: Option<WireFormat>) -> Option<u64> {
        let json = match (frame, format) {
            (Message::Text(text), _) if text.len() <= MAX_HEARTBEAT_PROBE_BYTES => text.as_str(),
            (Message::Binary(bytes), Some(WireFormat::GzipEnvelopeV1))
                if bytes.len() <= MAX_HEARTBEAT_PROBE_BYTES =>
            {
                let payload = bytes.strip_prefix(&[crate::wire::FORMAT_RAW])?;
                std::str::from_utf8(payload).ok()?
            }
            (Message::Text(_) | Message::Binary(_), _)
            | (Message::Ping(_) | Message::Pong(_) | Message::Close(_), _) => return None,
        };
        let now = tokio::time::Instant::now();
        let refill = (now.duration_since(self.refilled_at).as_millis()
            / PROBE_REFILL_INTERVAL.as_millis())
        .min(u128::from(FAST_PATH_BURST)) as u32;
        if refill > 0 {
            self.tokens = (self.tokens + refill).min(FAST_PATH_BURST);
            self.refilled_at = if self.tokens == FAST_PATH_BURST {
                now
            } else {
                self.refilled_at + PROBE_REFILL_INTERVAL * refill
            };
        }
        if self.tokens == 0 {
            return None;
        }
        let probe: PingProbe = match serde_json::from_str(json) {
            Ok(probe) => probe,
            Err(_) => {
                self.tokens -= 1;
                return None;
            }
        };
        match probe.kind {
            MessageKind::Ping => {
                self.tokens -= 1;
                probe.data.timestamp
            }
            MessageKind::Other => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp(frame: &Message, format: Option<WireFormat>) -> Option<u64> {
        HeartbeatProbe::default().timestamp(frame, format)
    }

    #[test]
    fn only_valid_top_level_ping_can_bypass_command_admission() {
        assert_eq!(
            timestamp(
                &Message::text(r#"{"type":"Ping","data":{"timestamp":4}}"#),
                None
            ),
            Some(4)
        );
        for json in [
            r#"{"type":"CreateGame","data":{"timestamp":4},"note":"Ping"}"#,
            r#"{"type":"Ping","data":{"timestamp":-1}}"#,
            r#"{"type":"Ping","data":{"timestamp":"4"}}"#,
            r#"{"type":"Ping","data":{}}"#,
            r#"{"type":"Ping","type":"CreateGame","data":{"timestamp":4}}"#,
        ] {
            assert_eq!(timestamp(&Message::text(json), None), None, "{json}");
        }
    }

    #[test]
    fn plain_and_negotiated_raw_controls_accept_json_field_order_and_whitespace() {
        let json = r#"{ "data": { "timestamp": 4 }, "type": "Ping" }"#;
        assert_eq!(timestamp(&Message::text(json), None), Some(4));
        let mut raw = vec![crate::wire::FORMAT_RAW];
        raw.extend_from_slice(json.as_bytes());
        let frame = Message::binary(raw);
        assert_eq!(timestamp(&frame, None), None);
        assert_eq!(timestamp(&frame, Some(WireFormat::GzipEnvelopeV1)), Some(4));
        let mut compressed = vec![1];
        compressed.extend_from_slice(json.as_bytes());
        assert_eq!(
            timestamp(
                &Message::binary(compressed),
                Some(WireFormat::GzipEnvelopeV1)
            ),
            None
        );
    }

    #[test]
    fn oversized_valid_pings_do_not_enter_the_reader_probe() {
        let ping = r#"{"type":"Ping","data":{"timestamp":4}}"#;
        let boundary = format!(
            "{ping}{}",
            " ".repeat(MAX_HEARTBEAT_PROBE_BYTES - ping.len())
        );
        assert_eq!(timestamp(&Message::text(boundary.clone()), None), Some(4));
        assert_eq!(
            timestamp(&Message::text(format!("{boundary} ")), None),
            None
        );
        let mut raw = vec![crate::wire::FORMAT_RAW];
        raw.extend_from_slice(boundary.trim_end().as_bytes());
        raw.resize(MAX_HEARTBEAT_PROBE_BYTES, b' ');
        assert_eq!(
            timestamp(
                &Message::binary(raw.clone()),
                Some(WireFormat::GzipEnvelopeV1)
            ),
            Some(4)
        );
        raw.push(b' ');
        assert_eq!(
            timestamp(&Message::binary(raw), Some(WireFormat::GzipEnvelopeV1)),
            None
        );
    }

    #[tokio::test(start_paused = true)]
    async fn probe_burst_is_bounded_and_refills_without_idle_credit_above_capacity() {
        let mut probe = HeartbeatProbe::default();
        let ping = Message::text(r#"{"type":"Ping","data":{"timestamp":4}}"#);
        for _ in 0..FAST_PATH_BURST {
            assert_eq!(probe.timestamp(&ping, None), Some(4));
        }
        assert_eq!(probe.timestamp(&ping, None), None);
        tokio::time::advance(PROBE_REFILL_INTERVAL - std::time::Duration::from_millis(1)).await;
        assert_eq!(probe.timestamp(&ping, None), None);
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        assert_eq!(probe.timestamp(&ping, None), Some(4));
        assert_eq!(probe.timestamp(&ping, None), None);
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        for _ in 0..FAST_PATH_BURST {
            assert_eq!(probe.timestamp(&ping, None), Some(4));
        }
        assert_eq!(probe.timestamp(&ping, None), None);
    }

    #[tokio::test(start_paused = true)]
    async fn malformed_probes_spend_the_heartbeat_allowance() {
        let mut probe = HeartbeatProbe::default();
        for _ in 0..FAST_PATH_BURST {
            assert_eq!(probe.timestamp(&Message::text("{"), None), None);
        }
        let ping = Message::text(r#"{"type":"Ping","data":{"timestamp":4}}"#);
        assert_eq!(probe.timestamp(&ping, None), None);
    }

    #[tokio::test(start_paused = true)]
    async fn valid_non_ping_commands_preserve_the_heartbeat_reserve() {
        let mut probe = HeartbeatProbe::default();
        let action = Message::text(
            serde_json::to_string(&server_core::protocol::ClientMessage::Action {
                action: engine::types::actions::GameAction::PassPriority,
            })
            .unwrap(),
        );
        for _ in 0..40 {
            assert_eq!(probe.timestamp(&action, None), None);
        }
        let ping = Message::text(r#"{"type":"Ping","data":{"timestamp":4}}"#);
        for _ in 0..FAST_PATH_BURST {
            assert_eq!(probe.timestamp(&ping, None), Some(4));
        }
        assert_eq!(probe.timestamp(&ping, None), None);
    }
}
