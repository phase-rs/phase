import { beforeEach, describe, it, expect, vi } from "vitest";

import { getDiagnosticHistory, getDiagnosticSources } from "../../services/troubleshooting";
import { trackEvent } from "../../services/telemetry";
import { buildGameState } from "../../test/factories/gameStateFactory";
import { createPeerSession } from "../peer";
import type { PeerSessionOptions } from "../peer";
import { validateMessage } from "../protocol";
import type { P2PMessage } from "../protocol";
import { FakeDataConnection } from "./fakeDataConnection";

vi.mock("../../services/telemetry", () => ({ trackEvent: vi.fn() }));
beforeEach(() => { vi.mocked(trackEvent).mockClear(); });

function createTestSession(opts?: PeerSessionOptions) {
  const conn = new FakeDataConnection();
  // Cast to satisfy DataConnection type — we only use the subset FakeDataConnection implements.
  const session = createPeerSession(conn as never, opts);
  return { conn, session };
}

/**
 * `simulateData` encodes plain objects, so it can only ever produce a DECODE
 * failure. The other drop site takes a frame that never was binary — what an
 * old-bundle peer sending plain JSON objects puts on the wire — so capture the
 * transport's own data handler and deliver one verbatim.
 */
class RawFrameConnection extends FakeDataConnection {
  private readonly rawHandlers = new Set<(data: unknown) => void>();
  override on(event: string, handler: (...args: unknown[]) => void): this {
    if (event === "data") this.rawHandlers.add(handler as (data: unknown) => void);
    return super.on(event, handler);
  }

  async deliverRaw(data: unknown): Promise<void> {
    await Promise.allSettled([...this.rawHandlers].map((h) => h(data)));
  }
}

// Drain all pending microtasks/timers so the recvQueue-chained pending-message
// flush (scheduled by onMessage) has run.
const flushAsync = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

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
  it("reports a dropped send and bypasses encoding when connection is not open", async () => {
    const { conn, session } = createTestSession();
    conn.open = false;
    await expect(session.send({ type: "concede" })).resolves.toBe(false);
    expect(conn.sentRaw.length).toBe(0);
    session.close();
  });

  it("reports a dropped queued send when the channel closes before its write", async () => {
    const { conn, session } = createTestSession();
    const send = session.send({ type: "concede" });
    conn.open = false;

    await expect(send).resolves.toBe(false);
    expect(conn.sentRaw).toEqual([]);
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
    await flushAsync();

    expect(handler).toHaveBeenCalledTimes(1);
    expect(handler).toHaveBeenCalledWith(actionMessage);
    session.close();
  });

  it("awaits async pending handlers and dispatches buffered messages before later inbound ones", async () => {
    const { conn, session } = createTestSession();
    const order: string[] = [];

    const buffered = {
      type: "action" as const,
      senderPlayerId: 0,
      action: { type: "PassPriority" as const },
    };
    // Buffered before any handler is attached.
    await conn.simulateData(buffered);

    const handler = vi.fn(async (m: P2PMessage) => {
      await Promise.resolve();
      order.push(m.type);
    });
    session.onMessage(handler);
    await flushAsync();
    // A later inbound message must be dispatched only after the buffered one.
    await conn.simulateData({ type: "concede" });

    expect(order).toEqual(["action", "concede"]);
    session.close();
  });

  it("keeps the session alive when a pending-message handler throws", async () => {
    const { conn, session } = createTestSession();
    await conn.simulateData({ type: "concede" });

    const received: string[] = [];
    const handler = vi.fn((m: P2PMessage) => {
      received.push(m.type);
      if (received.length === 1) {
        throw new Error("boom on first pending message");
      }
    });
    session.onMessage(handler);
    await flushAsync();
    // The thrown pending handler must not poison recvQueue: later inbound
    // messages still reach the handler.
    await conn.simulateData({ type: "concede" });

    expect(received).toEqual(["concede", "concede"]);
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
    expect(trackEvent).toHaveBeenCalledExactlyOnceWith("p2p_disconnect", expect.objectContaining({
      reason: "connection-close", channel_open: false,
    }));
  });

  it("reports remote closure without collecting the remote's free-form reason", async () => {
    const { conn } = createTestSession();
    await conn.simulateData({ type: "disconnect", reason: "private room information" });
    expect(trackEvent).toHaveBeenCalledExactlyOnceWith("p2p_disconnect", expect.objectContaining({
      reason: "remote-disconnect", last_message_type: "disconnect",
    }));
    expect(JSON.stringify(vi.mocked(trackEvent).mock.calls)).not.toContain("private room information");
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

    await expect(session.send({ type: "concede" })).resolves.toBe(false);

    expect(onDisconnect).toHaveBeenCalledTimes(1);
    expect(onDisconnect).toHaveBeenCalledWith("Channel send failed");
    errorSpy.mockRestore();
  });

  // A wire-version skew must TRAVERSE the transport, not die inside it. This
  // file mocks only telemetry, so the REAL `encodeWireMessage`/`decodeWireMessage`
  // and the real binary framing run end to end — the one place in the suite
  // where that is true. A `game_setup` stamped with a stale version used to
  // throw inside `decodeWireMessage` and be swallowed by the decode `catch`
  // above: frame dropped, channel open, nothing downstream told, guest hung.
  // The version rule now lives solely in the adapter, so the frame must
  // arrive at an `onMessage` handler for that rule to be able to run at all.
  it("delivers a stale-wire-version game_setup to the message handler", async () => {
    const { conn, session } = createTestSession();
    const handler = vi.fn();
    session.onMessage(handler);

    await conn.simulateData({
      type: "game_setup",
      wireProtocolVersion: 25,
      assignedPlayerId: 1,
      playerToken: "token-123",
      state: buildGameState(),
      events: [],
      legalActions: [],
      manaPaymentShortcutActions: [],
    });

    expect(handler).toHaveBeenCalledTimes(1);
    expect(handler).toHaveBeenCalledWith(
      expect.objectContaining({ type: "game_setup", wireProtocolVersion: 25 }),
    );
    session.close();
  });

  // The frames that CANNOT traverse the transport — an unknown envelope version
  // byte from a newer peer, a plain object from an older bundle — used to die
  // here with a `console.warn` and nothing downstream told. Only the adapter
  // holds the state to decide what a lost frame costs, so the transport has to
  // say it lost one.
  it("reports both undeliverable inbound frames instead of swallowing them", async () => {
    const onUndeliverableFrame = vi.fn();
    const conn = new RawFrameConnection();
    const session = createPeerSession(conn as never, { onUndeliverableFrame });
    const handler = vi.fn();
    session.onMessage(handler);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});

    await conn.simulateData(new Uint8Array([0x99, 0x01, 0x02]));
    await conn.deliverRaw({ type: "concede" });

    expect(onUndeliverableFrame.mock.calls).toEqual([["decode-failed"], ["non-binary"]]);
    // Neither frame reached a handler; the neighbour above proves a frame that
    // decodes does, so this is the drop and not a dead session.
    expect(handler).not.toHaveBeenCalled();

    // A frame arriving after `close()` reports nothing: both sites sit below
    // `if (closed) return`, and a closed session has no adapter left to tell.
    session.close();
    onUndeliverableFrame.mockClear();
    await conn.simulateData(new Uint8Array([0x99, 0x01, 0x02]));
    await conn.deliverRaw({ type: "concede" });

    expect(onUndeliverableFrame).not.toHaveBeenCalled();
    warn.mockRestore();
  });
});

/**
 * A channel whose peer never answers a ping — i.e. a HALF-OPEN channel:
 * `open` stays true, `conn.on("close")` never fires, and nothing comes back.
 * The keep-alive silence check is the only thing that can detect it.
 */
class SilentPeerConnection extends FakeDataConnection {
  protected override onDecodedSend(): void {}
}

// Fake timers are scoped per-test: the rest of this file runs on real timers
// and would be slowed (or made order-dependent) by a suite-wide fake clock.
//
// `ping`/`pong` are far below `WIRE_COMPRESSION_THRESHOLD`, so they take the
// raw envelope path — no `CompressionStream`, which is what lets the real
// protocol module run under fake timers here.
describe("PeerSession keep-alive", () => {
  it.each([false, true])("processes heartbeats during a slow game handler (buffered: %s)", async (buffered) => {
    vi.useFakeTimers();
    const { conn, session } = createTestSession();
    let release!: () => void;
    const blocked = new Promise<void>((resolve) => { release = resolve; });
    try {
      const onDisconnect = vi.fn();
      session.onDisconnect(onDisconnect);
      const received: string[] = [];
      const action: P2PMessage = {
        type: "action", senderPlayerId: 1, action: { type: "PassPriority" },
      };
      if (buffered) await conn.simulateData(action);
      session.onMessage(async (msg) => {
        received.push(msg.type);
        if (msg.type === "action") await blocked;
      });
      const first = buffered ? Promise.resolve() : conn.simulateData(action);
      await vi.advanceTimersByTimeAsync(0);
      expect(received).toEqual(["action"]);
      const second = conn.simulateData({ type: "concede" });

      // Both directions of liveness must bypass game work. The fake answers
      // our periodic pings; this inbound ping also requires us to answer it.
      const inboundPing = conn.simulateData({ type: "ping", timestamp: 123 });
      await vi.advanceTimersByTimeAsync(30_000);
      expect(onDisconnect).not.toHaveBeenCalled();
      expect(conn.open).toBe(true);
      expect(await conn.getSentMessages()).toContainEqual({ type: "pong", timestamp: 123 });
      expect(received).toEqual(["action"]);

      release();
      await Promise.all([first, second, inboundPing]);
      expect(received).toEqual(["action", "concede"]);
    } finally {
      release();
      session.close();
      vi.useRealTimers();
    }
  });

  it("keeps a silent peer connected during a slow game handler", async () => {
    vi.useFakeTimers();
    const conn = new SilentPeerConnection();
    const session = createPeerSession(conn as never);
    let release!: () => void;
    const blocked = new Promise<void>((resolve) => { release = resolve; });
    try {
      const onDisconnect = vi.fn();
      session.onDisconnect(onDisconnect);
      const received: string[] = [];
      session.onMessage(async (msg) => {
        received.push(msg.type);
        await blocked;
      });
      const first = conn.simulateData({ type: "concede" });
      await vi.advanceTimersByTimeAsync(0);
      const second = conn.simulateData({ type: "concede" });
      await vi.advanceTimersByTimeAsync(10_000);
      expect(onDisconnect).not.toHaveBeenCalled();

      release();
      await Promise.all([first, second]);
      expect(received).toEqual(["concede", "concede"]);
    } finally {
      release();
      session.close();
      vi.useRealTimers();
    }
  });

  it("marks latency stale without closing a channel that stops answering pings", async () => {
    vi.useFakeTimers();
    const conn = new SilentPeerConnection();
    const onLatency = vi.fn();
    const session = createPeerSession(conn as never, { onLatency });
    try {
      const onDisconnect = vi.fn();
      session.onDisconnect(onDisconnect);
      await conn.simulateData({ type: "pong", timestamp: Date.now() - 42 });
      expect(onLatency).toHaveBeenLastCalledWith(42);
      await vi.advanceTimersByTimeAsync(15_000);
      expect(onLatency).toHaveBeenLastCalledWith(null);
      await conn.simulateData({ type: "emote", emote: "still here" });
      await vi.advanceTimersByTimeAsync(45_000);
      expect(onDisconnect).not.toHaveBeenCalled();
      expect(conn.open).toBe(true);
      expect(trackEvent).not.toHaveBeenCalled();
      await conn.simulateData({ type: "pong", timestamp: Date.now() - 21 });
      expect(onLatency).toHaveBeenLastCalledWith(21);
      conn.close();
      expect(onDisconnect).toHaveBeenCalledExactlyOnceWith("Connection closed");
    } finally {
      session.close();
      vi.useRealTimers();
    }
  });

  it("keeps a ponging peer connected indefinitely", async () => {
    vi.useFakeTimers();
    try {
      const conn = new FakeDataConnection();
      const session = createPeerSession(conn as never);
      const onDisconnect = vi.fn();
      session.onDisconnect(onDisconnect);

      // Six ticks' worth — three times the silence budget.
      await vi.advanceTimersByTimeAsync(30_000);

      expect(onDisconnect).not.toHaveBeenCalled();
      // The pongs are real: they arrived over the wire in response to pings.
      const sent = await conn.getSentMessages();
      expect(sent.filter((m) => (m as P2PMessage).type === "ping").length).toBe(6);
      session.close();
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not disconnect a healthy peer across a suspend/resume clock jump", async () => {
    vi.useFakeTimers();
    try {
      const conn = new FakeDataConnection();
      const session = createPeerSession(conn as never);
      const onDisconnect = vi.fn();
      session.onDisconnect(onDisconnect);

      await vi.advanceTimersByTimeAsync(10_000);
      expect(onDisconnect).not.toHaveBeenCalled();

      // Suspend: move the wall clock five minutes WITHOUT running the
      // interval. `setSystemTime` shifts pending timers' deadlines along with
      // the clock, so no tick fires — which is what a frozen tab looks like.
      // `advanceTimersByTime(300_000)` would instead run sixty ticks that each
      // observe a healthy 5s gap, and would pass against any implementation.
      vi.setSystemTime(Date.now() + 300_000);

      // Resume: elapsed wall time must never tear down the channel.
      await vi.advanceTimersByTimeAsync(5_000);

      expect(onDisconnect).not.toHaveBeenCalled();
      session.close();
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not disconnect when the wall clock moves backward", async () => {
    vi.useFakeTimers();
    try {
      const conn = new SilentPeerConnection();
      const session = createPeerSession(conn as never);
      const onDisconnect = vi.fn();
      session.onDisconnect(onDisconnect);

      // One healthy tick: 5s of silence, not yet the budget.
      await vi.advanceTimersByTimeAsync(5_000);
      expect(onDisconnect).not.toHaveBeenCalled();

      // The wall clock steps BACKWARD a minute (NTP correction, manual clock
      // change, VM restore). `setSystemTime` moves pending deadlines with it,
      // so ticks keep their 5s spacing — only `Date.now()` jumps.
      vi.setSystemTime(Date.now() - 60_000);

      // Missing pongs and clock adjustments affect measurement only.
      await vi.advanceTimersByTimeAsync(20_000);

      expect(onDisconnect).not.toHaveBeenCalled();
      session.close();
    } finally {
      vi.useRealTimers();
    }
  });
});


it("retains pre-teardown evidence after native state mutation and PeerJS field clearing", () => {
  const pc = Object.assign(new EventTarget(), { connectionState: "connected", iceConnectionState: "completed" });
  const channel = Object.assign(new EventTarget(), { readyState: "open", bufferedAmount: 2048 });
  const removePc = vi.spyOn(pc, "removeEventListener");
  const removeChannel = vi.spyOn(channel, "removeEventListener");
  const conn = Object.assign(new FakeDataConnection(), { peerConnection: pc as typeof pc | null, dataChannel: channel as typeof channel | null });
  const before = getDiagnosticSources().peers.length;
  const session = createPeerSession(conn as never);
  channel.dispatchEvent(new Event("error"));
  pc.connectionState = "closed";
  pc.iceConnectionState = "closed";
  channel.readyState = "closed";
  channel.bufferedAmount = 0;
  pc.dispatchEvent(new Event("connectionstatechange"));
  conn.peerConnection = null;
  conn.dataChannel = null;
  conn.simulateClose();
  session.close();
  expect(trackEvent).toHaveBeenCalledTimes(1);
  expect(trackEvent).toHaveBeenCalledWith("p2p_disconnect", expect.objectContaining({ connection_state: "closed", last_connection_state: "connected", last_ice_state: "completed", last_channel_state: "open", last_buffered_bytes: 2048, channel_error: "data-channel-error" }));
  expect(getDiagnosticHistory().slice(-1)[0]).toMatchObject({ kind: "disconnect", preClose: { bufferedBytes: 2048, receiveAgeMs: null, pongAgeMs: null } });
  expect(getDiagnosticSources().peers).toHaveLength(before);
  expect(removePc).toHaveBeenCalledTimes(2);
  expect(removeChannel).toHaveBeenCalledTimes(4);
});


it("retains the selected route after disconnect and ignores late stats", async () => {
  const report = new Map<string, Record<string, unknown>>([
    ["transport", { type: "transport", selectedCandidatePairId: "pair" }],
    ["pair", { type: "candidate-pair", localCandidateId: "local", remoteCandidateId: "remote" }],
    ["local", { candidateType: "relay", protocol: "udp", address: "SECRET" }],
    ["remote", { candidateType: "host", protocol: "udp" }],
  ]);
  let complete: ((value: unknown) => void) | undefined;
  const getStats = vi.fn().mockResolvedValueOnce(report).mockImplementation(() => new Promise((resolve) => { complete = resolve; }));
  const pc = Object.assign(new EventTarget(), { connectionState: "connected", iceConnectionState: "completed", getStats });
  const conn = Object.assign(new FakeDataConnection(), { peerConnection: pc });
  const session = createPeerSession(conn as never);
  await new Promise((resolve) => setTimeout(resolve, 0));
  pc.dispatchEvent(new Event("connectionstatechange"));
  await Promise.resolve();
  conn.simulateClose();
  const before = getDiagnosticHistory();
  expect(before.slice(-1)[0]).toMatchObject({ kind: "disconnect", candidates: { localType: "relay", remoteType: "host", localProtocol: "udp", observedAt: expect.any(Number) } });
  complete?.(report);
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(getDiagnosticHistory()).toEqual(before);
  expect(JSON.stringify(before)).not.toContain("SECRET");
  expect(getStats).toHaveBeenCalledTimes(2);
  session.close();
});


it("does not start deferred route stats after immediate teardown or native closure", async () => {
  const getStats = vi.fn().mockResolvedValue(new Map());
  const pc = Object.assign(new EventTarget(), { connectionState: "connected", iceConnectionState: "completed", getStats });
  const conn = Object.assign(new FakeDataConnection(), { peerConnection: pc });
  const session = createPeerSession(conn as never);
  session.close();
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(getStats).not.toHaveBeenCalled();

  const next = createPeerSession(Object.assign(new FakeDataConnection(), { peerConnection: pc }) as never);
  pc.connectionState = "closed";
  pc.dispatchEvent(new Event("connectionstatechange"));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(getStats).not.toHaveBeenCalled();
  next.close();
});

it("preserves typed established-connection errors with the same diagnostic identity", () => {
  const conn = new FakeDataConnection();
  const session = createPeerSession(conn as never);
  const source = getDiagnosticSources().peers.slice(-1)[0];
  conn.simulateError(Object.assign(new Error("SECRET"), { type: "negotiation-failed" }));
  expect(getDiagnosticHistory().slice(-1)[0]).toMatchObject({ kind: "disconnect", diagnosticId: source.diagnosticId, error: "negotiation-failed" });
  expect(JSON.stringify(getDiagnosticHistory().slice(-1)[0])).not.toContain("SECRET");
  session.close();
});

it("coalesces route changes during an in-flight probe and shares the refreshed evidence", async () => {
  const selected = new Map<string, Record<string, unknown>>([
    ["transport", { type: "transport", selectedCandidatePairId: "pair" }],
    ["pair", { localCandidateId: "local" }],
    ["local", { candidateType: "relay", protocol: "udp", relayProtocol: "tcp" }],
  ]);
  let finish!: (value: Map<string, Record<string, unknown>>) => void;
  const getStats = vi.fn().mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; })).mockResolvedValue(selected);
  const pc = Object.assign(new EventTarget(), { connectionState: "connecting", iceConnectionState: "checking", getStats });
  const conn = Object.assign(new FakeDataConnection(), { peerConnection: pc });
  const session = createPeerSession(conn as never);
  await Promise.resolve();
  pc.connectionState = "connected";
  pc.iceConnectionState = "completed";
  pc.dispatchEvent(new Event("connectionstatechange"));
  pc.dispatchEvent(new Event("iceconnectionstatechange"));
  const pending = getDiagnosticSources().peers.slice(-1)[0].stats();
  finish(new Map());
  expect(await pending).toMatchObject({ localType: "relay", relayProtocol: "tcp" });
  expect(getStats).toHaveBeenCalledTimes(2);
  conn.simulateClose();
  expect(getDiagnosticHistory().slice(-1)[0]).toMatchObject({ kind: "disconnect", candidates: { localType: "relay", relayProtocol: "tcp" } });
  session.close();
});
