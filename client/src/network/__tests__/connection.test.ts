import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import { logSelectedIceCandidate } from "../connection";

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
