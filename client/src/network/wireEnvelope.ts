// Shared binary JSON envelope used by both WebRTC DataChannels and the
// client/server WebSocket transport:
//   [0x00][raw UTF-8 JSON]
//   [0x01][gzip-compressed UTF-8 JSON]

// Keep this aligned with crates/phase-server/src/wire.rs.
export const WIRE_COMPRESSION_THRESHOLD = 256;
export const WIRE_MAX_DECODED_BYTES = 1024 * 1024;
export type WireFormat = "GzipEnvelopeV1";

const FORMAT_RAW = 0x00;
const FORMAT_GZIP = 0x01;

export function supportsGzipEnvelope(): boolean {
  return typeof CompressionStream !== "undefined"
    && typeof DecompressionStream !== "undefined";
}

export async function encodeJsonEnvelope(json: string): Promise<Uint8Array> {
  const jsonBytes = new TextEncoder().encode(json);
  if (jsonBytes.length < WIRE_COMPRESSION_THRESHOLD) {
    const out = new Uint8Array(1 + jsonBytes.length);
    out[0] = FORMAT_RAW;
    out.set(jsonBytes, 1);
    return out;
  }

  const stream = new Blob([jsonBytes]).stream().pipeThrough(new CompressionStream("gzip"));
  const gzipped = new Uint8Array(await new Response(stream).arrayBuffer());
  const out = new Uint8Array(1 + gzipped.length);
  out[0] = FORMAT_GZIP;
  out.set(gzipped, 1);
  return out;
}

export async function decodeJsonEnvelope(bytes: Uint8Array): Promise<string> {
  if (bytes.length < 1) throw new Error("empty wire message");

  const format = bytes[0];
  const payload = bytes.subarray(1);
  if (format === FORMAT_RAW) {
    if (payload.length > WIRE_MAX_DECODED_BYTES) {
      throw new Error("wire message exceeds decoded size limit");
    }
    return new TextDecoder("utf-8", { fatal: true }).decode(payload);
  }
  if (format === FORMAT_GZIP) {
    const stream = new Blob([payload]).stream().pipeThrough(new DecompressionStream("gzip"));
    const reader = stream.getReader();
    const decoder = new TextDecoder("utf-8", { fatal: true });
    let decodedBytes = 0;
    let decoded = "";

    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        decodedBytes += value.byteLength;
        if (decodedBytes > WIRE_MAX_DECODED_BYTES) {
          throw new Error("wire message exceeds decoded size limit");
        }
        decoded += decoder.decode(value, { stream: true });
      }
      return decoded + decoder.decode();
    } catch (error) {
      await reader.cancel(error).catch(() => undefined);
      throw error;
    } finally {
      reader.releaseLock();
    }
  }
  throw new Error(`unknown wire format version: 0x${format.toString(16)}`);
}
