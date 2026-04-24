import { describe, it, expect } from "vitest";

import { decodeWireMessage, encodeWireMessage, validateMessage } from "../protocol";
import type { P2PMessage } from "../protocol";

describe("encodeWireMessage / decodeWireMessage", () => {
  // (a) Round-trip across P2PMessage variants.
  const variants: P2PMessage[] = [
    { type: "ping", timestamp: 12345 },
    { type: "pong", timestamp: 12345 },
    { type: "concede" },
    { type: "disconnect", reason: "Page closed" },
    { type: "kick", reason: "Removed" },
    { type: "host_left", reason: "Host left" },
    { type: "player_kicked", playerId: 2, reason: "Removed" },
    { type: "player_conceded", playerId: 1, reason: "Conceded" },
    { type: "player_disconnected", playerId: 1 },
    { type: "player_reconnected", playerId: 1 },
    { type: "game_paused", reason: "Player disconnected" },
    { type: "game_resumed" },
    { type: "lobby_progress", joined: 1, total: 3 },
    { type: "emote", emote: "🔥" },
    { type: "reconnect", playerToken: "token-123" },
    { type: "reconnect_rejected", reason: "Unknown token" },
    { type: "action_rejected", reason: "Player kicked" },
    {
      type: "action",
      senderPlayerId: 0,
      action: { type: "PassPriority" },
    },
  ];

  it.each(variants)("round-trips %j", async (msg) => {
    const bytes = await encodeWireMessage(msg);
    const out = await decodeWireMessage(bytes);
    expect(out).toEqual(msg);
  });

  // (b) Tiny messages take FORMAT_RAW.
  it("ping uses FORMAT_RAW (0x00) — too small for gzip to win", async () => {
    const bytes = await encodeWireMessage({ type: "ping", timestamp: 1 });
    expect(bytes[0]).toBe(0x00);
  });

  // (c) Large messages take FORMAT_GZIP and produce a smaller-than-raw payload.
  // Don't assert on a specific compression ratio — DEFLATE tuning varies.
  it("large messages take FORMAT_GZIP and shrink relative to raw JSON", async () => {
    const bigPayload = "x".repeat(2000);
    const msg = {
      type: "action",
      senderPlayerId: 0,
      action: { type: "PassPriority", padding: bigPayload },
    } as unknown as P2PMessage;
    const bytes = await encodeWireMessage(msg);
    expect(bytes[0]).toBe(0x01); // FORMAT_GZIP
    const rawSize = new TextEncoder().encode(JSON.stringify(msg)).length;
    expect(bytes.length).toBeLessThan(rawSize);
  });

  // (d) Unknown version byte rejects cleanly.
  it("rejects unknown version byte", async () => {
    const bytes = new Uint8Array([0xff, 0x01, 0x02]);
    await expect(decodeWireMessage(bytes)).rejects.toThrow(/unknown wire format/);
  });

  it("rejects empty payload", async () => {
    await expect(decodeWireMessage(new Uint8Array())).rejects.toThrow(/empty/);
  });

  // (e) Compressed payload still gates through validateMessage so unknown
  // message types are rejected, not silently passed through.
  it("decode runs validateMessage — unknown type rejected", async () => {
    const fake = { type: "definitely_not_a_real_type", x: 1 };
    const json = JSON.stringify(fake);
    const stream = new Blob([new TextEncoder().encode(json)])
      .stream()
      .pipeThrough(new CompressionStream("gzip"));
    const gz = new Uint8Array(await new Response(stream).arrayBuffer());
    const bytes = new Uint8Array(1 + gz.length);
    bytes[0] = 0x01;
    bytes.set(gz, 1);
    await expect(decodeWireMessage(bytes)).rejects.toThrow(/Invalid message type/);
  });
});

describe("validateMessage", () => {
  it("accepts known types", () => {
    expect(validateMessage({ type: "concede" })).toEqual({ type: "concede" });
  });
  it("rejects missing type", () => {
    expect(() => validateMessage({ foo: "bar" })).toThrow(/missing type/);
  });
  it("rejects unknown type", () => {
    expect(() => validateMessage({ type: "nope" })).toThrow(/Invalid message type/);
  });
});
