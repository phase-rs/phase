import { describe, it, expect, vi } from "vitest";

import { createPeerSession } from "../peer";
import { validateMessage } from "../protocol";
import { FakeDataConnection } from "./fakeDataConnection";

function createTestSession(opts?: { onSessionEnd?: () => void }) {
  const conn = new FakeDataConnection();
  // Cast to satisfy DataConnection type — we only use the subset FakeDataConnection implements.
  const session = createPeerSession(conn as never, opts);
  return { conn, session };
}

describe("P2P Protocol - validateMessage", () => {
  it("accepts valid P2P message types", () => {
    const msg = {
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    };
    expect(validateMessage(msg)).toEqual(msg);

    const concede = { type: "concede" };
    expect(validateMessage(concede)).toEqual(concede);

    const ping = { type: "ping", timestamp: 12345 };
    expect(validateMessage(ping)).toEqual(ping);
  });

  it("accepts new 3-4p multiplayer message types", () => {
    const types = [
      { type: "reconnect", playerToken: "abc" },
      { type: "reconnect_rejected", reason: "kicked" },
      { type: "kick", reason: "host kicked" },
      { type: "player_kicked", playerId: 2, reason: "kicked" },
      { type: "player_conceded", playerId: 2, reason: "Conceded" },
      { type: "player_disconnected", playerId: 1 },
      { type: "player_reconnected", playerId: 1 },
      { type: "game_paused", reason: "Player disconnected" },
      { type: "game_resumed" },
      { type: "lobby_progress", joined: 2, total: 3 },
    ];
    for (const msg of types) {
      expect(validateMessage(msg)).toEqual(msg);
    }
  });

  it("rejects unknown message types", () => {
    expect(() => validateMessage({ type: "unknown_garbage" })).toThrow(
      "Invalid message type",
    );
  });

  it("rejects missing type field", () => {
    expect(() => validateMessage({})).toThrow("Invalid message: missing type field");
    expect(() => validateMessage(null)).toThrow("Invalid message: missing type field");
    expect(() => validateMessage("not an object")).toThrow(
      "Invalid message: missing type field",
    );
  });
});

describe("PeerSession", () => {
  it("send resolves immediately and bypasses encoding when connection is not open", async () => {
    const { conn, session } = createTestSession();
    conn.open = false;
    // The closed-channel sentinel is now a same-microtask resolve with no
    // bytes recorded — equivalent to the old `false` return.
    await session.send({ type: "concede" });
    expect(conn.sentRaw.length).toBe(0);
    session.close();
  });

  it("onMessage handler receives parsed messages", async () => {
    const { conn, session } = createTestSession();
    const handler = vi.fn();
    session.onMessage(handler);

    const actionMessage = {
      type: "action" as const,
      senderPlayerId: 0,
      action: { type: "PassPriority" as const },
    };
    await conn.simulateData(actionMessage);

    expect(handler).toHaveBeenCalledTimes(1);
    expect(handler).toHaveBeenCalledWith(actionMessage);
    session.close();
  });

  it("buffers messages when no listeners are attached, then flushes on subscribe", async () => {
    const { conn, session } = createTestSession();

    const actionMessage = {
      type: "action" as const,
      senderPlayerId: 0,
      action: { type: "PassPriority" as const },
    };
    await conn.simulateData(actionMessage);

    const handler = vi.fn();
    session.onMessage(handler);

    expect(handler).toHaveBeenCalledTimes(1);
    expect(handler).toHaveBeenCalledWith(actionMessage);
    session.close();
  });

  it("invokes disconnect handlers immediately if subscribed after disconnect", () => {
    const onSessionEnd = vi.fn();
    const { session } = createTestSession({ onSessionEnd });

    session.close("Peer closed");

    const handler = vi.fn();
    session.onDisconnect(handler);

    expect(handler).toHaveBeenCalledTimes(1);
    expect(handler).toHaveBeenCalledWith("Peer closed");
    expect(onSessionEnd).toHaveBeenCalledTimes(1);
  });

  it("onSessionEnd fires exactly once per session, even on cascading errors", () => {
    const onSessionEnd = vi.fn();
    const { conn, session } = createTestSession({ onSessionEnd });

    conn.simulateClose();
    conn.simulateClose(); // duplicate
    session.close("manual"); // additional close attempt

    expect(onSessionEnd).toHaveBeenCalledTimes(1);
  });

  // Regression: a thrown handler MUST NOT poison the recvQueue. `.then()`
  // without a rejection handler propagates rejection forward, so a single
  // exception would otherwise silently freeze inbound dispatch for the
  // remainder of the session. The fix wraps each handler invocation in an
  // internal try/catch, mirroring the sendQueue posture.
  it("recvQueue continues dispatching after a handler throws", async () => {
    const { conn, session } = createTestSession();
    const errorSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const calls: number[] = [];
    let throwOnNext = true;
    session.onMessage(() => {
      calls.push(calls.length);
      if (throwOnNext) {
        throwOnNext = false;
        throw new Error("handler boom");
      }
    });

    await conn.simulateData({ type: "concede" });
    await conn.simulateData({ type: "concede" });
    await conn.simulateData({ type: "concede" });

    // All three messages must still reach the handler — the first throw
    // must not silence the queue.
    expect(calls.length).toBe(3);
    errorSpy.mockRestore();
    session.close();
  });

  // Regression for plan test (g): when `conn.send` throws synchronously
  // inside the queued send entry, the session must call `handleDisconnect`
  // — the keep-alive's pong-timeout is the safety net but immediate
  // detection is the documented contract.
  it("conn.send throwing inside the queue triggers handleDisconnect", async () => {
    const { conn, session } = createTestSession();
    const onDisconnect = vi.fn();
    session.onDisconnect(onDisconnect);
    const errorSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    // Replace `send` with a throwing impl. Any send routed through the
    // queue will hit this and trigger handleDisconnect from the queued
    // catch — same disconnect semantics the original sync path provided.
    conn.send = () => {
      throw new Error("channel torn down");
    };

    await session.send({ type: "concede" });

    expect(onDisconnect).toHaveBeenCalledTimes(1);
    expect(onDisconnect).toHaveBeenCalledWith("Channel send failed");
    errorSpy.mockRestore();
  });
});
