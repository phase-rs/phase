use std::io::{Read, Write};

use axum::extract::ws::Message;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

// Keep this aligned with client/src/network/wireEnvelope.ts.
const FORMAT_RAW: u8 = 0x00;
const FORMAT_GZIP: u8 = 0x01;
const COMPRESSION_THRESHOLD: usize = 256;
/// Deflate level for the outgoing envelope, and the only place it is chosen. Measured
/// across real four-seat state frames, level 3 buys most of the bytes level 6 does for a
/// fraction of its CPU. For scale: `flate2::Compression::fast()` is level 1, `::default()`
/// is 6.
const COMPRESSION_LEVEL: u32 = 3;

/// This envelope is the single compression authority on the egress path: gzip output is
/// incompressible, so a transport-level compressor above it (RFC 7692 permessage-deflate)
/// would spend CPU for nothing and must not be enabled.
pub async fn encode_json_message(json: String, use_envelope: bool) -> Result<Message, String> {
    if !use_envelope {
        return Ok(Message::text(json));
    }

    let bytes = json.into_bytes();
    if bytes.len() < COMPRESSION_THRESHOLD {
        let mut framed = Vec::with_capacity(bytes.len() + 1);
        framed.push(FORMAT_RAW);
        framed.extend(bytes);
        return Ok(Message::binary(framed));
    }

    let framed = tokio::task::spawn_blocking(move || {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::new(COMPRESSION_LEVEL));
        encoder
            .write_all(&bytes)
            .map_err(|error| error.to_string())?;
        let compressed = encoder.finish().map_err(|error| error.to_string())?;
        let mut framed = Vec::with_capacity(compressed.len() + 1);
        framed.push(FORMAT_GZIP);
        framed.extend(compressed);
        Ok::<_, String>(framed)
    })
    .await
    .map_err(|error| error.to_string())??;

    Ok(Message::binary(framed))
}

pub async fn decode_client_envelope(
    bytes: Vec<u8>,
    max_json_bytes: usize,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || decode_envelope(&bytes, max_json_bytes))
        .await
        .map_err(|error| error.to_string())?
}

fn decode_envelope(bytes: &[u8], max_json_bytes: usize) -> Result<String, String> {
    let (&format, payload) = bytes
        .split_first()
        .ok_or_else(|| "empty binary envelope".to_string())?;
    let decoded = match format {
        FORMAT_RAW => payload.to_vec(),
        FORMAT_GZIP => {
            let mut decoded = Vec::new();
            GzDecoder::new(payload)
                .take((max_json_bytes + 1) as u64)
                .read_to_end(&mut decoded)
                .map_err(|error| format!("invalid gzip envelope: {error}"))?;
            decoded
        }
        other => return Err(format!("unknown binary envelope format: {other:#04x}")),
    };
    if decoded.len() > max_json_bytes {
        return Err("decompressed WebSocket message exceeds limit".to_string());
    }
    String::from_utf8(decoded).map_err(|error| format!("envelope is not UTF-8 JSON: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use server_core::protocol::ServerMessage;

    async fn encode_server_message(
        message: &ServerMessage,
        use_envelope: bool,
    ) -> Result<Message, String> {
        let json = serde_json::to_string(message).map_err(|error| error.to_string())?;
        encode_json_message(json, use_envelope).await
    }

    #[test]
    fn raw_and_gzip_envelopes_decode() {
        let raw = [vec![FORMAT_RAW], br#"{"type":"Ping"}"#.to_vec()].concat();
        assert_eq!(decode_envelope(&raw, 1024).unwrap(), r#"{"type":"Ping"}"#);

        let body = vec![b'x'; 1024];
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&body).unwrap();
        let gzip = [vec![FORMAT_GZIP], encoder.finish().unwrap()].concat();
        assert_eq!(decode_envelope(&gzip, 1024).unwrap().into_bytes(), body);
    }

    #[test]
    fn rejects_unknown_and_oversized_envelopes() {
        assert!(decode_envelope(&[0xff, 1], 8)
            .unwrap_err()
            .contains("unknown"));

        assert!(decode_envelope(&[FORMAT_GZIP, 1, 2, 3], 8)
            .unwrap_err()
            .contains("invalid gzip"));

        let oversized = [vec![FORMAT_RAW], vec![b'x'; 9]].concat();
        assert!(decode_envelope(&oversized, 8)
            .unwrap_err()
            .contains("exceeds"));

        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&[b'x'; 9]).unwrap();
        let oversized_gzip = [vec![FORMAT_GZIP], encoder.finish().unwrap()].concat();
        assert!(decode_envelope(&oversized_gzip, 8)
            .unwrap_err()
            .contains("exceeds"));
    }

    /// A body whose gzip output differs at every deflate level 0-9. Repeated JSON structure
    /// gives the matcher something to find; varied field values make how far it looks change
    /// what it finds. A uniform body such as `"x".repeat(512)` gzips to identical bytes from
    /// level 2 through 8, which is what a level assertion must not be built on.
    fn level_discriminating_payload() -> String {
        let mut seed = 0x9e37_79b9u32;
        let mut payload = String::new();
        while payload.len() < 14_700 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = seed >> 8;
            payload.push_str(&format!(
                "{{\"seat\":{},\"object\":{},\"name\":\"Card {}\",\"tags\":[{},{}]}},",
                n % 4,
                n % 9973,
                n % 251,
                n % 1_000_003,
                n % 65_521
            ));
        }
        payload
    }

    #[tokio::test]
    async fn gzip_envelope_uses_the_named_compression_level() {
        let json =
            serde_json::to_string(&ServerMessage::error(level_discriminating_payload())).unwrap();
        let Message::Binary(framed) = encode_json_message(json.clone(), true).await.unwrap() else {
            panic!("negotiated large frame must use the binary envelope");
        };
        assert_eq!(framed[0], FORMAT_GZIP);

        let body_at = |level: u32| {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::new(level));
            encoder.write_all(json.as_bytes()).unwrap();
            encoder.finish().unwrap()
        };
        let expected = body_at(COMPRESSION_LEVEL);
        for level in (0..=9u32).filter(|level| *level != COMPRESSION_LEVEL) {
            assert_ne!(
                body_at(level),
                expected,
                "this payload cannot tell level {level} from level {COMPRESSION_LEVEL}"
            );
        }
        // Byte equality against the const's own level. The loop above is what makes it
        // discriminating: on a uniform payload every level from 2 to 8 emits the same bytes,
        // so the same assertion would pin a neighbourhood rather than the const the encoder
        // is supposed to read.
        assert_eq!(
            &framed[1..],
            expected.as_slice(),
            "envelope body is not gzip level {COMPRESSION_LEVEL}"
        );
        assert_eq!(decode_envelope(&framed, json.len()).unwrap(), json);
    }

    #[tokio::test]
    async fn outgoing_frames_preserve_text_fallback_and_select_envelope_format() {
        let small = ServerMessage::Pong { timestamp: 7 };
        assert!(matches!(
            encode_server_message(&small, false).await.unwrap(),
            Message::Text(_)
        ));

        let raw = encode_server_message(&small, true).await.unwrap();
        let Message::Binary(raw) = raw else {
            panic!("negotiated small frame must use the raw binary envelope");
        };
        assert_eq!(raw[0], FORMAT_RAW);
        assert!(decode_envelope(&raw, 1024).unwrap().contains("Pong"));

        let large = ServerMessage::error("x".repeat(COMPRESSION_THRESHOLD * 2));
        let gzip = encode_server_message(&large, true).await.unwrap();
        let Message::Binary(gzip) = gzip else {
            panic!("negotiated large frame must use gzip");
        };
        assert_eq!(gzip[0], FORMAT_GZIP);
        assert!(decode_envelope(&gzip, 4096)
            .unwrap()
            .contains(&"x".repeat(64)));
    }
}
