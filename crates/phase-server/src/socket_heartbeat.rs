//! Recognize application heartbeats before retaining application commands.
//!
//! client/src/network/wireEnvelope.ts uses a 256-byte compression threshold;
//! its ~40-byte Ping is always plain text or FORMAT_RAW. Compressed envelopes
//! deliberately remain subject to bounded decoder work admission.

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
pub(crate) const PROBE_WORK_BURST: u32 = 64;
const WORK_REFILL_INTERVAL: std::time::Duration = std::time::Duration::from_micros(15_625);

struct Credit {
    tokens: u32,
    refilled_at: tokio::time::Instant,
}

impl Credit {
    fn refill(&mut self, now: tokio::time::Instant, capacity: u32, interval: std::time::Duration) {
        let refill = (now.duration_since(self.refilled_at).as_nanos() / interval.as_nanos())
            .min(u128::from(capacity)) as u32;
        if refill > 0 {
            self.tokens = (self.tokens + refill).min(capacity);
            self.refilled_at = if self.tokens == capacity {
                now
            } else {
                self.refilled_at + interval * refill
            };
        }
    }

    fn take(&mut self) -> bool {
        if let Some(remaining) = self.tokens.checked_sub(1) {
            self.tokens = remaining;
            true
        } else {
            false
        }
    }
}

enum ProbeInput<'a> {
    Text(&'a str),
    Raw(&'a [u8]),
}

/// Every probe parse spends work credit, including valid non-Pings. A separate
/// reply reserve prevents ordinary command bursts from spending Ping credit.
pub(crate) struct HeartbeatProbe {
    work: Credit,
    heartbeats: Credit,
    #[cfg(test)]
    parses: usize,
    #[cfg(test)]
    observer: Option<std::sync::Arc<crate::socket_test_hooks::ParserWork>>,
}

impl Default for HeartbeatProbe {
    fn default() -> Self {
        let now = tokio::time::Instant::now();
        Self {
            work: Credit {
                tokens: PROBE_WORK_BURST,
                refilled_at: now,
            },
            heartbeats: Credit {
                tokens: FAST_PATH_BURST,
                refilled_at: now,
            },
            #[cfg(test)]
            parses: 0,
            #[cfg(test)]
            observer: None,
        }
    }
}

impl HeartbeatProbe {
    #[cfg(test)]
    pub(crate) fn observe(&mut self, work: std::sync::Arc<crate::socket_test_hooks::ParserWork>) {
        self.work.refilled_at = work.probe_at();
        self.heartbeats.refilled_at = work.probe_at();
        self.observer = Some(work);
    }

    pub(crate) fn timestamp(&mut self, frame: &Message, format: Option<WireFormat>) -> Option<u64> {
        let input = match (frame, format) {
            (Message::Text(text), _) if text.len() <= MAX_HEARTBEAT_PROBE_BYTES => {
                ProbeInput::Text(text.as_str())
            }
            (Message::Binary(bytes), Some(WireFormat::GzipEnvelopeV1))
                if bytes.len() <= MAX_HEARTBEAT_PROBE_BYTES =>
            {
                ProbeInput::Raw(bytes.strip_prefix(&[crate::wire::FORMAT_RAW])?)
            }
            (Message::Text(_) | Message::Binary(_), _)
            | (Message::Ping(_) | Message::Pong(_) | Message::Close(_), _) => return None,
        };
        #[cfg(test)]
        let now = self
            .observer
            .as_ref()
            .map_or_else(tokio::time::Instant::now, |work| work.probe_at());
        #[cfg(not(test))]
        let now = tokio::time::Instant::now();
        self.heartbeats
            .refill(now, FAST_PATH_BURST, PROBE_REFILL_INTERVAL);
        if self.heartbeats.tokens == 0 {
            return None;
        }
        self.work
            .refill(now, PROBE_WORK_BURST, WORK_REFILL_INTERVAL);
        if !self.work.take() {
            return None;
        }
        let json = match input {
            ProbeInput::Text(text) => text,
            ProbeInput::Raw(bytes) => match std::str::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => {
                    self.heartbeats.take();
                    return None;
                }
            },
        };
        #[cfg(test)]
        {
            self.parses += 1;
            if let Some(work) = &self.observer {
                work.probe_parse();
            }
        }
        let probe: PingProbe = match serde_json::from_str(json) {
            Ok(probe) => probe,
            Err(_) => {
                self.heartbeats.take();
                return None;
            }
        };
        match probe.kind {
            MessageKind::Ping => {
                self.heartbeats.take();
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
    async fn every_compact_non_ping_probe_is_charged_before_json_parse() {
        for format in [None, Some(WireFormat::GzipEnvelopeV1)] {
            let json = serde_json::to_string(&server_core::protocol::ClientMessage::SubscribeLobby)
                .unwrap();
            let frame = if format.is_some() {
                let mut bytes = vec![crate::wire::FORMAT_RAW];
                bytes.extend_from_slice(json.as_bytes());
                Message::binary(bytes)
            } else {
                Message::text(json)
            };
            let mut probe = HeartbeatProbe::default();
            for _ in 0..PROBE_WORK_BURST * 4 {
                assert_eq!(probe.timestamp(&frame, format), None);
            }
            assert_eq!(
                probe.parses, PROBE_WORK_BURST as usize,
                "unadmitted JSON parsing"
            );
            tokio::time::advance(WORK_REFILL_INTERVAL).await;
            assert_eq!(probe.timestamp(&frame, format), None);
            assert_eq!(probe.parses, PROBE_WORK_BURST as usize + 1);
        }
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
