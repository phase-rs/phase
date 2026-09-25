import { afterEach, describe, expect, it, vi } from "vitest";
import { boundedDiagnosticProbe, DIAGNOSTIC_HISTORY_LIMIT, getDiagnosticHistory, projectCandidateStats, recordDiagnostic, registerEngineDiagnostics, registerPeerDiagnostics, runDiagnostics, type DiagnosticEnvironment, type EngineDiagnosticSnapshot, type TransportDiagnosticSnapshot } from "../troubleshooting";

describe("safe local troubleshooting observations", () => {
  it("bounds history and returns detached facts", () => {
    const now = Date.now();
    for (let observedAt = 0; observedAt < 50; observedAt += 1) recordDiagnostic({
      kind: "engine-not-initialized", observedAt: now - 50 + observedAt, operation: "getState",
      engine: { observedAt, initialized: false, initializing: false, disposed: false, execution: "unavailable" },
    });
    const history = getDiagnosticHistory();
    expect(history).toHaveLength(DIAGNOSTIC_HISTORY_LIMIT);
    history[0].observedAt = -1;
    expect(getDiagnosticHistory()[0].observedAt).toBe(now - 30);
  });
  it("projects only selected candidate kinds and RTT, preserving unknown versus zero", () => {
    const report = new Map<string, Record<string, unknown>>([
      ["transport", { type: "transport", selectedCandidatePairId: "pair", dtlsCipher: "secret" }],
      ["pair", { type: "candidate-pair", localCandidateId: "local", remoteCandidateId: "remote", currentRoundTripTime: 0 }],
      ["local", { candidateType: "relay", address: "secret", url: "secret", usernameFragment: "secret" }],
      ["remote", { candidateType: "host", address: "secret" }],
    ]) as unknown as RTCStatsReport;
    expect(projectCandidateStats(report)).toEqual({ observedAt: expect.any(Number), localProtocol: null, relayProtocol: null, remoteProtocol: null, localType: "relay", remoteType: "host", roundTripMs: 0 });
    expect(projectCandidateStats(new Map() as unknown as RTCStatsReport)).toBeNull();
  });
  it("bounds hanging probes and clears successful timers", async () => {
    vi.useFakeTimers();
    try {
      await expect(boundedDiagnosticProbe(() => Promise.resolve(0))).resolves.toBe(0);
      expect(vi.getTimerCount()).toBe(0);
      const pending = expect(boundedDiagnosticProbe(() => new Promise(() => {}))).rejects.toThrow("Diagnostic timeout");
      await vi.advanceTimersByTimeAsync(1500);
      await pending;
      expect(vi.getTimerCount()).toBe(0);
    } finally { vi.useRealTimers(); }
  });
});

const environment: DiagnosticEnvironment = { version: "test", build: "build", mode: null, online: true, visibility: "visible", webAssembly: true, webRtc: true };
const engine: EngineDiagnosticSnapshot = { observedAt: 1, initialized: true, initializing: false, disposed: false, execution: "worker" };
const transport: TransportDiagnosticSnapshot = { observedAt: 1, connectionState: "connected", iceState: "connected", channelState: "open", bufferedBytes: 120, pendingSends: 1, pendingDecodes: 0, receiveAgeMs: 0, pongAgeMs: null, channelError: null };
const unregister: (() => void)[] = [];
afterEach(() => { unregister.splice(0).forEach((dispose) => dispose()); vi.unstubAllGlobals(); vi.useRealTimers(); });

describe("read-only diagnostic runner", () => {
  it("checks registered instances and relay without exposing endpoint, bodies, ping payload or secrets", async () => {
    const ping = vi.fn().mockResolvedValue({ game: "SECRET" });
    const stats = vi.fn().mockResolvedValue({ localType: "relay", remoteType: "host", roundTripMs: 0 });
    unregister.push(registerEngineDiagnostics({ snapshot: () => engine, ping }), registerPeerDiagnostics({ snapshot: () => transport, stats }));
    const fetcher = vi.fn().mockResolvedValue(new Response("SECRET"));
    vi.stubGlobal("fetch", fetcher);
    const report = await runDiagnostics({ ...environment, mode: "native-ai" }, "https://SECRET/servers", new AbortController().signal);
    expect(report.results).toEqual(expect.arrayContaining([
      expect.objectContaining({ reason: "engineReady", status: "pass" }),
      expect.objectContaining({ reason: "peerConnected", status: "pass" }),
      expect.objectContaining({ reason: "relayed", status: "pass" }),
      expect.objectContaining({ reason: "modeNotChecked", status: "unavailable" }),
    ]));
    expect(ping).toHaveBeenCalledTimes(1);
    expect(stats).toHaveBeenCalledTimes(1);
    expect(fetcher).toHaveBeenCalledExactlyOnceWith("https://SECRET/servers", expect.objectContaining({ method: "GET", credentials: "omit", cache: "no-store", referrerPolicy: "no-referrer" }));
    expect(JSON.stringify(report)).not.toContain("SECRET");
    expect(report.peers[0].transport.bufferedBytes).toBe(120);
    stats.mockResolvedValue({ localType: "srflx", remoteType: "host", roundTripMs: 12 });
    const draftReport = await runDiagnostics({ ...environment, mode: "draft-match" }, "/servers", new AbortController().signal);
    expect(draftReport.results).toEqual(expect.arrayContaining([
      expect.objectContaining({ check: "route", reason: "direct", status: "pass" }),
      expect.objectContaining({ check: "mode", reason: "draftModeNotChecked", status: "unavailable" }),
    ]));
    expect(draftReport.results).not.toContainEqual(expect.objectContaining({ reason: "modeNotChecked" }));
    stats.mockResolvedValue({ localType: null, remoteType: "host", roundTripMs: null });
    const unknownRoute = await runDiagnostics(environment, "/servers", new AbortController().signal);
    expect(unknownRoute.results).toContainEqual(expect.objectContaining({ check: "route", reason: "statsUnavailable", status: "unavailable" }));
    expect(unknownRoute.results).not.toContainEqual(expect.objectContaining({ reason: "direct" }));
  });
  it("labels absent sources unavailable and HTTP failure as a limited reachability warning", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null, { status: 503 })));
    const report = await runDiagnostics({ ...environment, online: false, visibility: "hidden", webRtc: false }, "/servers", new AbortController().signal);
    expect(report.results.map((result) => result.reason)).toEqual(expect.arrayContaining(["offline", "hidden", "unsupported", "noEngine", "noPeer", "directoryFailed"]));
    expect(report.results.filter((result) => result.reason === "noEngine" || result.reason === "noPeer").every((result) => result.status === "unavailable")).toBe(true);
  });
  it("does not ping an unready instance and contains rejected probes without copying error messages", async () => {
    const ping = vi.fn();
    unregister.push(registerEngineDiagnostics({ snapshot: () => ({ ...engine, initialized: false }), ping }), registerEngineDiagnostics({ snapshot: () => engine, ping: () => Promise.reject(new Error("SECRET")) }));
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("SECRET")));
    const report = await runDiagnostics(environment, "/servers", new AbortController().signal);
    expect(ping).not.toHaveBeenCalled();
    expect(report.results.map((result) => result.reason)).toEqual(expect.arrayContaining(["engineWaiting", "probeFailed", "directoryFailed"]));
    expect(JSON.stringify(report)).not.toContain("SECRET");
  });
  it("bounds hanging engine and directory checks and aborts the HTTP request", async () => {
    vi.useFakeTimers();
    unregister.push(registerEngineDiagnostics({ snapshot: () => engine, ping: () => new Promise(() => {}) }));
    const fetcher = vi.fn().mockImplementation(() => new Promise(() => {}));
    vi.stubGlobal("fetch", fetcher);
    const pending = runDiagnostics(environment, "/servers", new AbortController().signal);
    await vi.advanceTimersByTimeAsync(2500);
    const report = await pending;
    expect(report.results.map((result) => result.reason)).toEqual(expect.arrayContaining(["probeTimeout", "directoryFailed"]));
    expect(fetcher.mock.calls[0][1].signal.aborted).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
  });
  it("cancels promptly and does not start checks for an already cancelled run", async () => {
    const controller = new AbortController();
    const fetcher = vi.fn().mockImplementation(() => new Promise(() => {}));
    vi.stubGlobal("fetch", fetcher);
    const pending = expect(runDiagnostics(environment, "/servers", controller.signal)).rejects.toMatchObject({ name: "AbortError" });
    controller.abort();
    await pending;
    await expect(runDiagnostics(environment, "/servers", controller.signal)).rejects.toMatchObject({ name: "AbortError" });
    expect(fetcher).not.toHaveBeenCalled();
  });
});

it("reports a failed connection separately from unavailable candidate statistics", async () => {
  unregister.push(registerPeerDiagnostics({ snapshot: () => ({ ...transport, connectionState: "failed" }), stats: () => Promise.reject(new Error("SECRET")) }));
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null)));
  const report = await runDiagnostics(environment, "/servers", new AbortController().signal);
  expect(report.results).toEqual(expect.arrayContaining([
    expect.objectContaining({ check: "peer", reason: "peerFailed", status: "error" }),
    expect.objectContaining({ check: "route", reason: "statsUnavailable", status: "unavailable" }),
  ]));
  expect(JSON.stringify(report)).not.toContain("SECRET");
});

it("aborts an in-flight HTTP request when the dialog closes", async () => {
  const controller = new AbortController();
  const fetcher = vi.fn().mockImplementation(() => new Promise(() => {}));
  vi.stubGlobal("fetch", fetcher);
  const pending = expect(runDiagnostics(environment, "/servers", controller.signal)).rejects.toMatchObject({ name: "AbortError" });
  await Promise.resolve();
  expect(fetcher).toHaveBeenCalledTimes(1);
  controller.abort();
  await pending;
  expect(fetcher.mock.calls[0][1].signal.aborted).toBe(true);
});


it("uses nominated succeeded candidates only when no selected transport pair is available", () => {
  const report = new Map<string, Record<string, unknown>>([
    ["failed", { type: "candidate-pair", nominated: true, state: "failed", localCandidateId: "wrong" }],
    ["pair", { type: "candidate-pair", nominated: true, state: "succeeded", localCandidateId: "local" }],
    ["local", { candidateType: "srflx", protocol: "udp" }],
  ]) as unknown as RTCStatsReport;
  expect(projectCandidateStats(report)).toMatchObject({ localType: "srflx", localProtocol: "udp" });
});

it("composes the isolated network probe in parallel and presents its safe results first", async () => {
  const fetcher = vi.fn().mockResolvedValue(new Response(null));
  vi.stubGlobal("fetch", fetcher);
  let finish!: (results: Awaited<ReturnType<typeof runDiagnostics>>["results"]) => void;
  const probe = vi.fn().mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
  const controller = new AbortController();
  const pending = runDiagnostics(environment, "/servers", controller.signal, probe);
  await Promise.resolve();
  expect(fetcher).toHaveBeenCalledTimes(1);
  expect(probe).toHaveBeenCalledWith(controller.signal);
  finish([{ check: "relay", status: "pass", reason: "relayReady", observedAt: 1, evidence: { durationMs: 20 } }]);
  const report = await pending;
  expect(report.results[0]).toMatchObject({ check: "relay", evidence: { durationMs: 20 } });
});

it("rechecks each player after the relay probe and removes stale connected verdicts", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null)));
  let current = transport;
  const departed = { snapshot: () => current, stats: async () => null };
  const survivor = { snapshot: () => transport, stats: async () => null };
  const leave = registerPeerDiagnostics(departed);
  unregister.push(leave, registerPeerDiagnostics(survivor));
  let finish!: (value: []) => void;
  const pending = runDiagnostics(environment, "/servers", new AbortController().signal, () => new Promise((resolve) => { finish = resolve; }));
  current = { ...transport, connectionState: "closed", channelState: "closed" };
  leave();
  finish([]);
  const report = await pending;
  const playerResults = report.results.filter((result) => result.check === "peer");
  expect(playerResults).toEqual([
    expect.objectContaining({ status: "warning", reason: "peerDisconnectedDuringCheck", diagnosticId: report.peers[0].diagnosticId }),
    expect.objectContaining({ status: "pass", reason: "peerConnected", diagnosticId: report.peers[1].diagnosticId }),
  ]);
  expect(report.peers[0].diagnosticId).not.toBe(report.peers[1].diagnosticId);
  expect(report.peers[0].transport.connectionState).toBe("closed");
});

it.each(["udp", "tcp", "tls", "SECRET"])("captures allowlisted client-to-TURN transport %s separately from candidate protocol", (relayProtocol) => {
  const stats = new Map<string, Record<string, unknown>>([
    ["transport", { type: "transport", selectedCandidatePairId: "pair" }],
    ["pair", { localCandidateId: "local" }],
    ["local", { candidateType: "relay", protocol: "udp", relayProtocol }],
  ]) as unknown as RTCStatsReport;
  expect(projectCandidateStats(stats)).toMatchObject({ localProtocol: "udp", relayProtocol: relayProtocol === "SECRET" ? null : relayProtocol });
});

it("includes a connection registered while network checks were running", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null)));
  let finish!: (value: []) => void;
  const pending = runDiagnostics(environment, "/servers", new AbortController().signal, () => new Promise((resolve) => { finish = resolve; }));
  unregister.push(registerPeerDiagnostics({ snapshot: () => transport, stats: async () => null }));
  finish([]);
  const report = await pending;
  expect(report.results).toContainEqual(expect.objectContaining({ reason: "peerConnected" }));
  expect(report.results).not.toContainEqual(expect.objectContaining({ reason: "noPeer" }));
});

it("gives explicit transport failure precedence when a connected peer fails during checks", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null)));
  let current = transport;
  unregister.push(registerPeerDiagnostics({ snapshot: () => current, stats: async () => null }));
  let finish!: (value: []) => void;
  const pending = runDiagnostics(environment, "/servers", new AbortController().signal, () => new Promise((resolve) => { finish = resolve; }));
  current = { ...transport, connectionState: "failed", iceState: "failed" };
  finish([]);
  expect((await pending).results).toContainEqual(expect.objectContaining({ check: "peer", status: "error", reason: "peerFailed" }));
});

it("rejects undeclared fields before they enter in-memory history or reports", async () => {
  const before = getDiagnosticHistory();
  const secretTopLevel = { kind: "credentials" as const, observedAt: Date.now(), outcome: "fresh" as const, credential: "SECRET" };
  const secretNested = { kind: "candidate-route" as const, observedAt: Date.now(), candidates: { localType: "relay" as const, remoteType: null, roundTripMs: 1, address: "SECRET" } };
  recordDiagnostic(secretTopLevel);
  recordDiagnostic(secretNested);
  expect(getDiagnosticHistory()).toEqual(before);
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null)));
  expect(JSON.stringify(await runDiagnostics(environment, "/servers", new AbortController().signal))).not.toContain("SECRET");
});
