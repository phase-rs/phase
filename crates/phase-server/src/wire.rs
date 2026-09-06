use std::io::{Read, Write};

use axum::extract::ws::Message;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

// Keep this aligned with client/src/network/wireEnvelope.ts.
const FORMAT_RAW: u8 = 0x00;
const FORMAT_GZIP: u8 = 0x01;
const COMPRESSION_THRESHOLD: usize = 256;

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
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
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
