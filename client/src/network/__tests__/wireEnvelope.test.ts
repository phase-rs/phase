import { describe, expect, it } from "vitest";

import {
  decodeJsonEnvelope,
  encodeJsonEnvelope,
  WIRE_MAX_DECODED_BYTES,
} from "../wireEnvelope";

describe("wire envelope decoded-size limit", () => {
  it("rejects oversized or malformed raw content", async () => {
    const envelope = new Uint8Array(WIRE_MAX_DECODED_BYTES + 2);
    envelope[0] = 0x00;

    await expect(decodeJsonEnvelope(envelope)).rejects.toThrow(
      "wire message exceeds decoded size limit",
    );
    await expect(decodeJsonEnvelope(new Uint8Array([0x00, 0xff]))).rejects.toThrow();
  });

  it("rejects oversized or malformed gzip content", async () => {
    const json = "x".repeat(WIRE_MAX_DECODED_BYTES + 1);
    const envelope = await encodeJsonEnvelope(json);

    expect(envelope[0]).toBe(0x01);
    expect(envelope.byteLength).toBeLessThan(WIRE_MAX_DECODED_BYTES);
    await expect(decodeJsonEnvelope(envelope)).rejects.toThrow(
      "wire message exceeds decoded size limit",
    );
    const stream = new Blob([new Uint8Array([0xff])]).stream()
      .pipeThrough(new CompressionStream("gzip"));
    const compressed = new Uint8Array(await new Response(stream).arrayBuffer());
    await expect(decodeJsonEnvelope(new Uint8Array([0x01, ...compressed]))).rejects.toThrow();
  });
});
