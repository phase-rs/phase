import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// Shared with the hoisted `vi.mock("peerjs")` factory below so the dial made
// inside `joinRoom` is observable. `vi.mock` is hoisted above imports, so the
// factory cannot close over ordinary module scope.
const peerState = vi.hoisted(() => ({
  peersCreated: 0,
  peerHandlers: new Map<string, Set<(arg?: unknown) => void>>(),
  connHandlers: new Map<string, (arg?: unknown) => void>(),
  dials: [] as Array<{ peerId: string; options: unknown }>,
  destroyCalls: 0,
  reconnectCalls: 0,
  emitPeer: (_event: string, _arg?: unknown): void => {},
}));

vi.mock("peerjs", () => {
  class FakePeer {
    destroyed = false;
    disconnected = false;
    private onceHandlers = new Map<(arg?: unknown) => void, (arg?: unknown) => void>();
    constructor() {
      peerState.peersCreated += 1;
      peerState.emitPeer = (event, arg) => {
        if (event === "disconnected") this.disconnected = true;
        if (event === "open") this.disconnected = false;
        for (const handler of [...(peerState.peerHandlers.get(event) ?? [])]) handler(arg);
      };
    }
    on(event: string, handler: (arg?: unknown) => void): void {
      const handlers = peerState.peerHandlers.get(event) ?? new Set();
      handlers.add(handler);
      peerState.peerHandlers.set(event, handlers);
    }
    once(event: string, handler: (arg?: unknown) => void): void {
      const once = (arg?: unknown) => {
        this.off(event, handler);
        handler(arg);
      };
      this.onceHandlers.set(handler, once);
      this.on(event, once);
    }
    off(event: string, handler: (arg?: unknown) => void): void {
      peerState.peerHandlers.get(event)?.delete(this.onceHandlers.get(handler) ?? handler);
      this.onceHandlers.delete(handler);
    }
    destroy(): void {
      this.destroyed = true;
      peerState.destroyCalls += 1;
      peerState.emitPeer("close");
    }
    reconnect(): void {
      this.disconnected = false;
      peerState.reconnectCalls += 1;
    }
    connect(peerId: string, options: unknown): unknown {
      peerState.dials.push({ peerId, options });
      return {
        open: false,
        on: (event: string, handler: (arg?: unknown) => void) => {
          const previous = peerState.connHandlers.get(event);
          peerState.connHandlers.set(event, (arg) => { previous?.(arg); handler(arg); });
        },
      };
    }
  }
  return { default: FakePeer };
});

import { dialPeer, fetchFreshTurnConfig, safePeerError, PEER_CONNECT_OPTIONS, TURN_CREDENTIALS_URL, hostRoom, joinRoom, logSelectedIceCandidate } from "../connection";
import { resolveTurnCredentialsUrl } from "../../config/turnCredentials";
import { peerTransportFactory } from "../transport";
import type { PeerTransportFactory, TransportPeer, TransportPeerOptions } from "../transport";

import { getDiagnosticHistory } from "../../services/troubleshooting";

// Fake RTCStatsReport: a Map<string, {type, ...}> with a forEach that matches
// the browser API shape.
function fakeStats(reports: Array<Record<string, unknown>>): RTCStatsReport {
  const map = new Map<string, Record<string, unknown>>();
  for (const r of reports) map.set(r.id as string, r);
  return {
    forEach(cb: (value: Record<string, unknown>) => void) {
      map.forEach((v) => cb(v));
    },
  } as unknown as RTCStatsReport;
}

function fakeConn(stats: RTCStatsReport | Error): {
  peerConnection: Pick<RTCPeerConnection, "getStats">;
} {
  return {
    peerConnection: {
      getStats: async () => {
        if (stats instanceof Error) throw stats;
        return stats;
      },
    } as Pick<RTCPeerConnection, "getStats">,
  };
}

type FakeTransportHandler = (value?: unknown) => void;

class FakeTransportEmitter {
  private readonly handlers = new Map<string, Set<FakeTransportHandler>>();
  private readonly onceWrappers = new Map<FakeTransportHandler, FakeTransportHandler>();

  on(event: string, handler: FakeTransportHandler): this {
    const handlers = this.handlers.get(event) ?? new Set<FakeTransportHandler>();
    handlers.add(handler);
    this.handlers.set(event, handlers);
    return this;
  }

  once(event: string, handler: FakeTransportHandler): this {
    const wrapped: FakeTransportHandler = (value) => {
      this.off(event, handler);
      handler(value);
    };
    this.onceWrappers.set(handler, wrapped);
    return this.on(event, wrapped);
  }

  off(event: string, handler: FakeTransportHandler): this {
    const registered = this.onceWrappers.get(handler) ?? handler;
    this.handlers.get(event)?.delete(registered);
    this.onceWrappers.delete(handler);
    return this;
  }

  emit(event: string, value?: unknown): void {
    for (const handler of [...(this.handlers.get(event) ?? [])]) handler(value);
  }

  clear(): void {
    this.handlers.clear();
    this.onceWrappers.clear();
  }
}

interface FakeTransportPeerHandle {
  peer: TransportPeer;
  connections: Array<{ peerId: string; options: unknown; emit: (event: string, value?: unknown) => void }>;
  emit: (event: string, value?: unknown) => void;
  destroy: ReturnType<typeof vi.fn>;
  reconnect: ReturnType<typeof vi.fn>;
}

interface FakeTransportCreation {
  id?: string;
  options?: TransportPeerOptions;
  handle: FakeTransportPeerHandle;
}

function makeFakeTransportFactory(creations: FakeTransportCreation[]): PeerTransportFactory {
  return {
    create(id, options) {
      const peerEvents = new FakeTransportEmitter();
      const connections: FakeTransportPeerHandle["connections"] = [];
      const peer: Record<string, unknown> = {
        id: id ?? `fake-peer-${creations.length + 1}`,
        destroyed: false,
        disconnected: false,
        connect: vi.fn((peerId: string, connectOptions: unknown) => {
          const connectionEvents = new FakeTransportEmitter();
          const connection = {
            open: false,
            peer: peerId,
            peerConnection: null,
            dataChannel: null,
            send: vi.fn(),
            close: vi.fn(() => connectionEvents.emit("close")),
            on: (event: string, handler: FakeTransportHandler) => { connectionEvents.on(event, handler); return connection; },
            once: (event: string, handler: FakeTransportHandler) => { connectionEvents.once(event, handler); return connection; },
            off: (event: string, handler: FakeTransportHandler) => { connectionEvents.off(event, handler); return connection; },
          };
          connections.push({ peerId, options: connectOptions, emit: (event, value) => {
            if (event === "open") connection.open = true;
            connectionEvents.emit(event, value);
          } });
          return connection;
        }),
        destroy: undefined,
        reconnect: undefined,
        on: (event: string, handler: FakeTransportHandler) => { peerEvents.on(event, handler); return peer; },
        once: (event: string, handler: FakeTransportHandler) => { peerEvents.once(event, handler); return peer; },
        off: (event: string, handler: FakeTransportHandler) => { peerEvents.off(event, handler); return peer; },
      };
      const destroy = vi.fn(() => {
        peer.destroyed = true;
        peerEvents.emit("close");
        peerEvents.clear();
      });
      const reconnect = vi.fn(() => { peer.disconnected = false; });
      peer.destroy = destroy;
      peer.reconnect = reconnect;
      const handle: FakeTransportPeerHandle = {
        peer: peer as unknown as TransportPeer,
        connections,
        emit: (event, value) => {
          if (event === "open") peer.disconnected = false;
          if (event === "disconnected") peer.disconnected = true;
          peerEvents.emit(event, value);
        },
        destroy,
        reconnect,
      };
      creations.push({ id, options, handle });
      return handle.peer;
    },
  };
}

describe("logSelectedIceCandidate", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.spyOn(console, "log").mockImplementation(() => {});
    vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.spyOn(console, "debug").mockImplementation(() => {});
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("logs direct when both candidates are host", async () => {
    const conn = fakeConn(
      fakeStats([
        { id: "pair1", type: "candidate-pair", nominated: true, state: "succeeded",
          localCandidateId: "local1", remoteCandidateId: "remote1" },
        { id: "local1", type: "local-candidate", candidateType: "host", protocol: "udp" },
        { id: "remote1", type: "remote-candidate", candidateType: "host", protocol: "udp" },
      ]),
    );

    const promise = logSelectedIceCandidate("Host", conn);
    await vi.advanceTimersByTimeAsync(2000);
    await promise;

    const calls = (console.log as ReturnType<typeof vi.fn>).mock.calls;
    expect(calls.length).toBe(1);
    expect(calls[0][0]).toContain("local=host/udp");
    expect(calls[0][0]).toContain("remote=host/udp");
    expect(calls[0][0]).toContain("✓ direct");
  });

  it("logs relayed warning when remote candidate is relay", async () => {
    const conn = fakeConn(
      fakeStats([
        { id: "pair1", type: "candidate-pair", nominated: true, state: "succeeded",
          localCandidateId: "local1", remoteCandidateId: "remote1" },
        { id: "local1", type: "local-candidate", candidateType: "host", protocol: "udp" },
        { id: "remote1", type: "remote-candidate", candidateType: "relay", protocol: "udp" },
      ]),
    );

    const promise = logSelectedIceCandidate("Guest", conn);
    await vi.advanceTimersByTimeAsync(2000);
    await promise;

    const msg = (console.log as ReturnType<typeof vi.fn>).mock.calls[0][0] as string;
    expect(msg).toContain("RELAYED VIA TURN");
    expect(msg).toContain("remote=relay/udp");
  });

  it("does not throw when getStats rejects", async () => {
    const conn = fakeConn(new Error("getStats blew up"));

    const promise = logSelectedIceCandidate("Host", conn);
    await vi.advanceTimersByTimeAsync(2000);
    await expect(promise).resolves.toBeUndefined();

    const warnCalls = (console.warn as ReturnType<typeof vi.fn>).mock.calls;
    expect(warnCalls.length).toBe(1);
    expect(warnCalls[0][0]).toContain("getStats failed");
  });

  it("does nothing when peerConnection is absent", async () => {
    const conn = { peerConnection: undefined };

    const promise = logSelectedIceCandidate("Host", conn);
    await vi.advanceTimersByTimeAsync(2000);
    await promise;

    expect((console.log as ReturnType<typeof vi.fn>).mock.calls.length).toBe(0);
    expect((console.warn as ReturnType<typeof vi.fn>).mock.calls.length).toBe(0);
  });

  it("does nothing when no nominated candidate pair is found", async () => {
    const conn = fakeConn(
      fakeStats([
        { id: "pair1", type: "candidate-pair", nominated: false, state: "in-progress",
          localCandidateId: "local1", remoteCandidateId: "remote1" },
      ]),
    );

    const promise = logSelectedIceCandidate("Host", conn);
    await vi.advanceTimersByTimeAsync(2000);
    await promise;

    expect((console.log as ReturnType<typeof vi.fn>).mock.calls.length).toBe(0);
  });
});

// Characterization: `joinRoom` has always passed these options. The test exists
// so the guarantee is pinned rather than assumed — every revision guard
// downstream depends on the channel being ordered, and the option that produces
// that ordering is a single word inside an object literal.
describe("joinRoom", () => {
  beforeEach(() => {
    peerState.peersCreated = 0;
    peerState.peerHandlers.clear();
    peerState.connHandlers.clear();
    peerState.dials.length = 0;
    peerState.destroyCalls = 0;
    peerState.reconnectCalls = 0;
    vi.spyOn(console, "log").mockImplementation(() => {});
    vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.spyOn(console, "debug").mockImplementation(() => {});
    vi.stubGlobal("fetch", vi.fn(async () => ({
      ok: true,
      json: async () => ({ iceServers: [{ urls: "stun:stun.example:3478" }] }),
    })));
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  // Real timers: `joinRoom` awaits the ICE-credential fetch before it ever
  // constructs its `Peer`, so a macrotask hop is what flushes that chain.
  const flush = () => new Promise((r) => setTimeout(r, 0));

  it("dials the host peer with the shared ordered-channel connect options", async () => {
    const joined = joinRoom("ABCDE");
    await flush();

    expect(peerState.peersCreated).toBe(1);
    peerState.emitPeer("open");

    expect(peerState.dials).toEqual([
      { peerId: "phase2-ABCDE", options: PEER_CONNECT_OPTIONS },
    ]);
    expect(PEER_CONNECT_OPTIONS.reliable).toBe(true);

    peerState.connHandlers.get("open")!();
    await expect(joined).resolves.toMatchObject({ conn: { open: false } });
  });

  it("uses the caller's transport factory for guest construction", async () => {
    const created: Array<{ id?: string; options?: unknown }> = [];
    const transportFactory: PeerTransportFactory = {
      create(id, options) {
        created.push({ id, options });
        return peerTransportFactory.create(id, options);
      },
    };

    const joining = joinRoom("ABCDE", undefined, undefined, transportFactory);
    await flush();

    expect(created).toHaveLength(1);
    expect(created[0]).toMatchObject({
      id: undefined,
      options: { config: { iceServers: expect.any(Array) } },
    });
    peerState.emitPeer("open");
    peerState.connHandlers.get("open")!();
    await expect(joining).resolves.toMatchObject({ conn: { open: false } });
  });

  it.each(["socket-error", "socket-closed", "server-error", "unavailable-id"])(
    "keeps an established guest alive after signaling %s",
    async (type) => {
      const joining = joinRoom("ABCDE");
      await flush();
      peerState.emitPeer("open");
      peerState.connHandlers.get("open")!();
      const joined = await joining;
      peerState.emitPeer("error", Object.assign(new Error("signaling interrupted"), { type }));
      expect(peerState.destroyCalls).toBe(0);
      joined.destroyPeer();
    },
  );

  it("still rejects and destroys a peer when the initial join fails", async () => {
    const joining = joinRoom("ABCDE");
    await flush();
    peerState.emitPeer("error", Object.assign(new Error("not registered"), { type: "socket-error" }));
    await expect(joining).rejects.toThrow("Failed to connect: not registered");
    expect(peerState.destroyCalls).toBe(1);
    expect(getDiagnosticHistory()).toContainEqual(expect.objectContaining({ kind: "signaling", event: "error", error: "socket-error" }));
    expect(JSON.stringify(getDiagnosticHistory())).not.toContain("not registered");
  });

  it("recovers guest signaling with backoff without redialing the initial game connection", async () => {
    vi.useFakeTimers();
    const joining = joinRoom("ABCDE");
    await vi.advanceTimersByTimeAsync(0);
    peerState.emitPeer("open");
    peerState.connHandlers.get("open")!();
    const joined = await joining;
    peerState.emitPeer("disconnected");
    peerState.emitPeer("disconnected");
    await vi.advanceTimersByTimeAsync(1000);
    expect(peerState.reconnectCalls).toBe(1);
    peerState.emitPeer("disconnected");
    await vi.advanceTimersByTimeAsync(1999);
    expect(peerState.reconnectCalls).toBe(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(peerState.reconnectCalls).toBe(2);
    peerState.emitPeer("open");
    expect(peerState.dials).toHaveLength(1);
    peerState.emitPeer("disconnected");
    await vi.advanceTimersByTimeAsync(1000);
    expect(peerState.reconnectCalls).toBe(3);
    peerState.emitPeer("disconnected");
    joined.destroyPeer();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(peerState.reconnectCalls).toBe(3);
  });

  it("preserves a hosted room through signaling errors and cancels recovery on teardown", async () => {
    vi.useFakeTimers();
    const hosting = hostRoom(undefined, { preferredRoomCode: "ABCDE" });
    await vi.advanceTimersByTimeAsync(0);
    peerState.emitPeer("open");
    const host = await hosting;
    peerState.emitPeer("error", Object.assign(new Error("signaling interrupted"), { type: "socket-error" }));
    expect(peerState.destroyCalls).toBe(0);
    peerState.emitPeer("disconnected");
    await vi.advanceTimersByTimeAsync(1000);
    expect(peerState.reconnectCalls).toBe(1);
    peerState.emitPeer("disconnected");
    host.destroy();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(peerState.reconnectCalls).toBe(1);
  });

  it("reuses the injected transport factory for host registration retries", async () => {
    vi.useFakeTimers();
    const created: Array<{ id?: string; options?: unknown }> = [];
    const transportFactory: PeerTransportFactory = {
      create(id, options) {
        created.push({ id, options });
        return peerTransportFactory.create(id, options);
      },
    };

    const hosting = hostRoom(undefined, {
      preferredRoomCode: "ABCDE",
      transportFactory,
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(created).toHaveLength(1);
    expect(created[0]).toMatchObject({
      id: "phase2-ABCDE",
      options: { config: { iceServers: expect.any(Array) } },
    });

    peerState.emitPeer("error", Object.assign(new Error("room still registered"), { type: "unavailable-id" }));
    await vi.advanceTimersByTimeAsync(3_000);
    expect(created).toHaveLength(2);
    expect(created[1]).toMatchObject({
      id: "phase2-ABCDE",
      options: { config: { iceServers: expect.any(Array) } },
    });

    peerState.emitPeer("open");
    const host = await hosting;
    host.destroy();
  });
});

describe("bootstrap transport selector integration", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    vi.resetModules();
  });

  const turnResponse = () => ({
    ok: true,
    json: async () => ({ iceServers: [{ urls: "turn:turn.example.org:3478", username: "user", credential: "credential" }] }),
  });

  it("keeps concurrent host and guest selections local when ICE replies in reverse order", async () => {
    vi.resetModules();
    vi.useFakeTimers();
    const transport = await import("../transport");
    const connection = await import("../connection");
    const hostCreations: FakeTransportCreation[] = [];
    const guestCreations: FakeTransportCreation[] = [];
    const hostFactory = makeFakeTransportFactory(hostCreations);
    const guestFactory = makeFakeTransportFactory(guestCreations);
    const contexts: Array<{ role: "host" | "guest"; hostPeerId: string }> = [];
    const pendingFetches: Array<(response: ReturnType<typeof turnResponse>) => void> = [];
    vi.stubGlobal("fetch", vi.fn(() => new Promise((resolve) => pendingFetches.push(resolve))));
    vi.spyOn(console, "log").mockImplementation(() => {});
    vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.spyOn(console, "debug").mockImplementation(() => {});

    transport.installPeerTransportSelector((context) => {
      contexts.push(context);
      return context.role === "host" ? hostFactory : guestFactory;
    });

    const hosting = connection.hostRoom(undefined, { preferredRoomCode: "phase2-AAAAA" });
    const joining = connection.joinRoom("phase2-BBBBB");
    expect(contexts).toEqual([
      { role: "host", hostPeerId: "phase2-AAAAA" },
      { role: "guest", hostPeerId: "phase2-BBBBB" },
    ]);
    expect(pendingFetches).toHaveLength(2);

    // Let the guest's later ICE request finish first. Its operation must keep
    // the guest factory selected with its own host ID.
    pendingFetches[1](turnResponse());
    await vi.advanceTimersByTimeAsync(0);
    expect(guestCreations).toHaveLength(1);
    expect(hostCreations).toHaveLength(0);

    pendingFetches[0](turnResponse());
    await vi.advanceTimersByTimeAsync(0);
    expect(hostCreations).toHaveLength(1);
    expect(hostCreations[0].id).toBe("phase2-AAAAA");
    expect(guestCreations[0].id).toBeUndefined();
    expect(guestCreations[0].options?.config.iceServers).toEqual(expect.arrayContaining([
      expect.objectContaining({ urls: ["turn:turn.example.org:3478"] }),
    ]));

    hostCreations[0].handle.emit("open");
    const host = await hosting;
    guestCreations[0].handle.emit("open");
    expect(guestCreations[0].handle.connections[0].peerId).toBe("phase2-BBBBB");
    guestCreations[0].handle.connections[0].emit("open");
    const joined = await joining;

    hostCreations[0].handle.emit("disconnected");
    guestCreations[0].handle.emit("disconnected");
    await vi.advanceTimersByTimeAsync(1_000);
    expect(hostCreations[0].handle.reconnect).toHaveBeenCalledTimes(1);
    expect(guestCreations[0].handle.reconnect).toHaveBeenCalledTimes(1);
    expect(hostCreations).toHaveLength(1);
    expect(guestCreations).toHaveLength(1);
    expect(guestCreations[0].handle.connections).toHaveLength(1);
    expect(contexts).toHaveLength(2);

    // Pending recovery timers are owned by the existing peers and disappear
    // when both sessions are destroyed.
    hostCreations[0].handle.emit("disconnected");
    guestCreations[0].handle.emit("disconnected");

    host.destroy();
    joined.destroyPeer();
    expect(hostCreations[0].handle.destroy).toHaveBeenCalledTimes(1);
    expect(guestCreations[0].handle.destroy).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(hostCreations[0].handle.reconnect).toHaveBeenCalledTimes(1);
    expect(guestCreations[0].handle.reconnect).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("preserves compound draft IDs and passes their exact prefixed host ID to selection", async () => {
    vi.resetModules();
    vi.useFakeTimers();
    const transport = await import("../transport");
    const connection = await import("../connection");
    const hostCreations: FakeTransportCreation[] = [];
    const guestCreations: FakeTransportCreation[] = [];
    const hostFactory = makeFakeTransportFactory(hostCreations);
    const guestFactory = makeFakeTransportFactory(guestCreations);
    const contexts: Array<{ role: "host" | "guest"; hostPeerId: string }> = [];
    const roomCode = "Draft-commander-aBc123ef";
    const hostPeerId = `phase2-${roomCode}`;
    vi.stubGlobal("fetch", vi.fn(async () => turnResponse()));
    vi.spyOn(console, "log").mockImplementation(() => {});
    vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.spyOn(console, "debug").mockImplementation(() => {});
    transport.installPeerTransportSelector((context) => {
      contexts.push(context);
      return context.role === "host" ? hostFactory : guestFactory;
    });

    const hosting = connection.hostRoom(undefined, { preferredRoomCode: roomCode });
    const joining = connection.joinRoom(roomCode);
    expect(contexts).toEqual([
      { role: "host", hostPeerId },
      { role: "guest", hostPeerId },
    ]);
    await vi.advanceTimersByTimeAsync(0);
    expect(hostCreations).toHaveLength(1);
    expect(hostCreations[0].id).toBe(hostPeerId);
    expect(guestCreations).toHaveLength(1);

    hostCreations[0].handle.emit("open");
    const host = await hosting;
    guestCreations[0].handle.emit("open");
    expect(guestCreations[0].handle.connections[0].peerId).toBe(hostPeerId);
    guestCreations[0].handle.connections[0].emit("open");
    const guest = await joining;

    host.destroy();
    guest.destroyPeer();
    expect(contexts).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("selects once before ICE and reuses the exact factory, host ID and options across retries", async () => {
    vi.resetModules();
    vi.useFakeTimers();
    const transport = await import("../transport");
    const connection = await import("../connection");
    const creations: FakeTransportCreation[] = [];
    const factory = makeFakeTransportFactory(creations);
    const selector = vi.fn(() => factory);
    vi.stubGlobal("fetch", vi.fn(async () => turnResponse()));
    vi.spyOn(console, "log").mockImplementation(() => {});
    vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.spyOn(console, "debug").mockImplementation(() => {});
    transport.installPeerTransportSelector(selector);

    const hosting = connection.hostRoom(undefined, { preferredRoomCode: "ABCDE" });
    expect(selector).toHaveBeenCalledTimes(1);
    expect(selector).toHaveBeenCalledWith({ role: "host", hostPeerId: "phase2-ABCDE" });
    await vi.advanceTimersByTimeAsync(0);
    expect(creations).toHaveLength(1);

    creations[0].handle.emit("error", Object.assign(new Error("occupied"), { type: "unavailable-id" }));
    await vi.advanceTimersByTimeAsync(3_000);
    expect(creations).toHaveLength(2);
    expect(creations[0].id).toBe("phase2-ABCDE");
    expect(creations[1].id).toBe(creations[0].id);
    expect(creations[1].options).toBe(creations[0].options);
    expect(selector).toHaveBeenCalledTimes(1);

    creations[1].handle.emit("open");
    const host = await hosting;
    host.destroy();
    expect(creations[0].handle.destroy).toHaveBeenCalledTimes(1);
    expect(creations[1].handle.destroy).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does not lock on pre-aborted or invalid operations and preserves explicit join overrides", async () => {
    vi.resetModules();
    vi.useFakeTimers();
    const transport = await import("../transport");
    const connection = await import("../connection");
    const controller = new AbortController();
    controller.abort();
    await expect(connection.joinRoom("ABCDE", controller.signal)).rejects.toMatchObject({ name: "AbortError" });
    await expect(connection.hostRoom(controller.signal)).rejects.toMatchObject({ name: "AbortError" });
    await expect(connection.joinRoom("")).rejects.toThrow("Invalid room code");
    await expect(connection.hostRoom(undefined, { preferredRoomCode: "" })).rejects.toThrow("Invalid room code");

    const overrideCreations: FakeTransportCreation[] = [];
    const override = makeFakeTransportFactory(overrideCreations);
    const selectorCreations: FakeTransportCreation[] = [];
    const selectorFactory = makeFakeTransportFactory(selectorCreations);
    const selector = vi.fn(() => selectorFactory);
    vi.stubGlobal("fetch", vi.fn(async () => turnResponse()));
    vi.spyOn(console, "log").mockImplementation(() => {});
    vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.spyOn(console, "debug").mockImplementation(() => {});

    const joining = connection.joinRoom("ABCDE", undefined, undefined, override);
    // Installation after an explicit override proves that it did not lock or
    // replace the shared selection policy.
    transport.installPeerTransportSelector(selector);
    await vi.advanceTimersByTimeAsync(0);
    expect(overrideCreations).toHaveLength(1);
    expect(selector).not.toHaveBeenCalled();
    overrideCreations[0].handle.emit("open");
    overrideCreations[0].handle.connections[0].emit("open");
    const joined = await joining;
    expect(selectorCreations).toHaveLength(0);
    joined.destroyPeer();
    await vi.advanceTimersByTimeAsync(2_000);
    expect(vi.getTimerCount()).toBe(0);
  });
});


describe("strict fresh TURN credentials", () => {
  afterEach(() => vi.unstubAllGlobals());
  it("validates servers and forwards abort without caching or exporting secrets", async () => {
    const configuredEndpoint = resolveTurnCredentialsUrl(process.env.TURN_CREDENTIALS_URL);
    const controller = new AbortController();
    const fetcher = vi.fn().mockResolvedValue(new Response(JSON.stringify({ iceServers: [
      { urls: "stun:example.org:3478" },
      { urls: ["turn:example.org:3478", "turns:example.org:443?transport=tcp"], username: "SECRET", credential: "SECRET" },
    ] })));
    vi.stubGlobal("fetch", fetcher);
    const before = getDiagnosticHistory();
    expect(TURN_CREDENTIALS_URL).toBe(configuredEndpoint);
    expect((await fetchFreshTurnConfig(controller.signal)).iceServers).toHaveLength(2);
    expect(fetcher).toHaveBeenCalledWith(configuredEndpoint, expect.objectContaining({ signal: controller.signal, cache: "no-store" }));
    expect(getDiagnosticHistory()).toEqual(before);
  });
  it.each([
    [new Response(null, { status: 503 }), "http"],
    [new Response("invalid JSON"), "invalid"],
    [new Response(JSON.stringify({ iceServers: [{ urls: "stun:example.org" }] })), "no-turn"],
    [new Response(JSON.stringify({ iceServers: [{ urls: "turn:example.org", credential: "SECRET" }] })), "invalid"],
  ])("classifies failures without leaking response content", async (response, reason) => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(response));
    await expect(fetchFreshTurnConfig()).rejects.toMatchObject({ reason });
  });
  it("classifies network errors and abort separately", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("SECRET")));
    await expect(fetchFreshTurnConfig()).rejects.toMatchObject({ reason: "network", message: "TURN credentials: network" });
    const controller = new AbortController();
    controller.abort();
    await expect(fetchFreshTurnConfig(controller.signal)).rejects.toMatchObject({ reason: "aborted" });
    expect(safePeerError({ type: "SECRET", message: "SECRET" })).toBe("unknown");
    expect(safePeerError({ type: "peer-unavailable" })).toBe("peer-unavailable");
  });
});


describe("connection attempt observation", () => {
  afterEach(() => vi.useRealTimers());
  it("retains pre-open ICE errors and times out once without closing the channel", async () => {
    vi.useFakeTimers();
    const pc = new EventTarget();
    const listeners = new Map<string, () => void>();
    const conn = { peerConnection: pc, on: (event: string, callback: () => void) => listeners.set(event, callback), close: vi.fn() };
    const peer = { connect: vi.fn().mockReturnValue(conn) };
    expect(dialPeer(peer as never, "SECRET", 20)).toBe(conn);
    pc.dispatchEvent(Object.assign(new Event("icecandidateerror"), { errorCode: 701, url: "SECRET", errorText: "SECRET" }));
    await vi.advanceTimersByTimeAsync(20);
    expect(getDiagnosticHistory().slice(-3)).toEqual([
      expect.objectContaining({ kind: "connection-attempt", event: "started" }),
      expect.objectContaining({ kind: "ice-candidate-error", code: 701 }),
      expect.objectContaining({ kind: "connection-attempt", event: "timeout" }),
    ]);
    const before = getDiagnosticHistory();
    listeners.get("open")!();
    pc.dispatchEvent(Object.assign(new Event("icecandidateerror"), { errorCode: 701 }));
    expect(getDiagnosticHistory()).toEqual(before);
    expect(conn.close).not.toHaveBeenCalled();
    expect(JSON.stringify(before)).not.toContain("SECRET");
  });
  it("clears the observation timer on open and reports undefined dials safely", () => {
    vi.useFakeTimers();
    const listeners = new Map<string, () => void>();
    dialPeer({ connect: () => ({ on: (event: string, callback: () => void) => listeners.set(event, callback) }) } as never, "SECRET", 20);
    listeners.get("open")!();
    expect(vi.getTimerCount()).toBe(0);
    expect(getDiagnosticHistory().slice(-1)[0]).toMatchObject({ kind: "connection-attempt", event: "open" });
    expect(() => dialPeer({ connect: () => undefined } as never, "SECRET", 20)).toThrow("Peer connection could not be created");
    expect(getDiagnosticHistory().slice(-1)[0]).toMatchObject({ kind: "connection-attempt", event: "error" });
  });
});

it("correlates interleaved setup and ICE failures by anonymous connection and Peer identity", () => {
  const listeners = [new Map<string, (value?: unknown) => void>(), new Map<string, (value?: unknown) => void>()];
  const connections = listeners.map((handlers) => ({ peerConnection: Object.assign(new EventTarget(), { connectionState: "failed", iceConnectionState: "failed" }), on: (event: string, handler: (value?: unknown) => void) => handlers.set(event, handler) }));
  const peer = { connect: vi.fn().mockReturnValueOnce(connections[0]).mockReturnValueOnce(connections[1]) };
  dialPeer(peer as never, "SECRET-1", 1000);
  dialPeer(peer as never, "SECRET-2", 1000);
  connections[0].peerConnection.dispatchEvent(Object.assign(new Event("icecandidateerror"), { errorCode: 701 }));
  listeners[1].get("error")!({ type: "negotiation-failed", message: "SECRET" });
  listeners[0].get("error")!({ type: "connection-closed", message: "SECRET" });
  const events = getDiagnosticHistory().slice(-5);
  expect(events[0].diagnosticId).not.toBe(events[1].diagnosticId);
  expect(events[2].diagnosticId).toBe(events[0].diagnosticId);
  expect(events[3]).toMatchObject({ diagnosticId: events[1].diagnosticId, error: "negotiation-failed", state: { connectionState: "failed", iceState: "failed" } });
  expect(events[4]).toMatchObject({ diagnosticId: events[0].diagnosticId, error: "connection-closed" });
  expect(new Set(events.map((event) => event.peerDiagnosticId)).size).toBe(1);
  expect(JSON.stringify(events)).not.toContain("SECRET");
});
