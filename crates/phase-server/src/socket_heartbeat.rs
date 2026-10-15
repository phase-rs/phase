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

/// Borrow the input and skip unrelated fields without materializing a command.
pub(crate) fn timestamp(frame: &Message, format: Option<WireFormat>) -> Option<u64> {
    let json = match (frame, format) {
        (Message::Text(text), _) => text.as_str(),
        (Message::Binary(bytes), Some(WireFormat::GzipEnvelopeV1)) => {
            let payload = bytes.strip_prefix(&[crate::wire::FORMAT_RAW])?;
            std::str::from_utf8(payload).ok()?
        }
        (Message::Binary(_), None | Some(WireFormat::Unknown))
        | (Message::Ping(_) | Message::Pong(_) | Message::Close(_), _) => return None,
    };
    let probe: PingProbe = serde_json::from_str(json).ok()?;
    match probe.kind {
        MessageKind::Ping => probe.data.timestamp,
        MessageKind::Other => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
