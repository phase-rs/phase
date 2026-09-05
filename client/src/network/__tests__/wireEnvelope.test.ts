import { describe, expect, it } from "vitest";

import { decodeJsonEnvelope, encodeJsonEnvelope } from "../wireEnvelope";

describe("wire envelope decoding", () => {
  it("rejects malformed raw UTF-8", async () => {
    await expect(decodeJsonEnvelope(new Uint8Array([0x00, 0xff]))).rejects.toThrow();
  });

  it("rejects malformed gzip UTF-8", async () => {
    const stream = new Blob([new Uint8Array([0xff])]).stream()
      .pipeThrough(new CompressionStream("gzip"));
    const compressed = new Uint8Array(await new Response(stream).arrayBuffer());
    await expect(decodeJsonEnvelope(new Uint8Array([0x01, ...compressed]))).rejects.toThrow();
  });

  it("round-trips a multi-megabyte frame", async () => {
    // A full viewer game state is several MB of JSON; there is deliberately no
    // decoded-size ceiling, since one dropped every such frame.
    const json = JSON.stringify({ type: "state_update", state: "x".repeat(4 * 1024 * 1024) });
    const envelope = await encodeJsonEnvelope(json);
    expect(envelope[0]).toBe(0x01);
    await expect(decodeJsonEnvelope(envelope)).resolves.toBe(json);
  });
});
