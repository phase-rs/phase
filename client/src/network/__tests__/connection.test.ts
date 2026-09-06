import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// Shared with the hoisted `vi.mock("peerjs")` factory below so the dial made
// inside `joinRoom` is observable. `vi.mock` is hoisted above imports, so the
// factory cannot close over ordinary module scope.
const peerState = vi.hoisted(() => ({
  peersCreated: 0,
  peerHandlers: new Map<string, (arg?: unknown) => void>(),
  connHandlers: new Map<string, (arg?: unknown) => void>(),
  dials: [] as Array<{ peerId: string; options: unknown }>,
}));

vi.mock("peerjs", () => {
  class FakePeer {
    constructor() {
      peerState.peersCreated += 1;
    }
    on(event: string, handler: (arg?: unknown) => void): void {
      peerState.peerHandlers.set(event, handler);
    }
    once(event: string, handler: (arg?: unknown) => void): void {
      peerState.peerHandlers.set(event, handler);
    }
    off(): void {}
    destroy(): void {}
    connect(peerId: string, options: unknown): unknown {
      peerState.dials.push({ peerId, options });
      return {
        open: false,
        on: (event: string, handler: (arg?: unknown) => void) => {
          peerState.connHandlers.set(event, handler);
        },
      };
    }
  }
  return { default: FakePeer };
});

import { PEER_CONNECT_OPTIONS, joinRoom, logSelectedIceCandidate } from "../connection";

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
    vi.spyOn(console, "log").mockImplementation(() => {});
    vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.spyOn(console, "debug").mockImplementation(() => {});
    vi.stubGlobal("fetch", vi.fn(async () => ({
      ok: true,
      json: async () => ({ iceServers: [{ urls: "stun:stun.example:3478" }] }),
    })));
  });

  afterEach(() => {
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
    peerState.peerHandlers.get("open")!();

    expect(peerState.dials).toEqual([
      { peerId: "phase2-ABCDE", options: PEER_CONNECT_OPTIONS },
    ]);
    expect(PEER_CONNECT_OPTIONS.reliable).toBe(true);

    peerState.connHandlers.get("open")!();
    await expect(joined).resolves.toMatchObject({ conn: { open: false } });
  });
});
