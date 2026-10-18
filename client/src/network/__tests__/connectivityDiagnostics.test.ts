import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { runConnectivityDiagnostics } from "../connectivityDiagnostics";
import { fetchFreshTurnConfig, PEER_CONNECT_OPTIONS, TurnCredentialError } from "../connection";
import { getDiagnosticSources } from "../../services/troubleshooting";

const mocks = vi.hoisted(() => ({ create: vi.fn() }));
vi.mock("peerjs", () => ({ default: class { constructor(...args: unknown[]) { return mocks.create(...args); } } }));
vi.mock("../connection", async (original) => ({ ...await original<typeof import("../connection")>(), fetchFreshTurnConfig: vi.fn() }));
class Emitter {
  onceWrappers = new Map<(...args: never[]) => void, (...args: never[]) => void>();
  listeners = new Map<string, Set<(...args: never[]) => void>>();
  on(event: string, handler: (...args: never[]) => void) { const handlers = this.listeners.get(event) ?? new Set(); handlers.add(handler); this.listeners.set(event, handlers); }
  once(event: string, handler: (...args: never[]) => void) { const wrapped = (...args: never[]) => { this.off(event, wrapped); handler(...args); }; this.onceWrappers.set(handler, wrapped); this.on(event, wrapped); }
  off(event: string, handler: (...args: never[]) => void) { this.listeners.get(event)?.delete(this.onceWrappers.get(handler) ?? handler); this.onceWrappers.delete(handler); }
  emit(event: string, ...args: unknown[]) { this.listeners.get(event)?.forEach((handler) => handler(...args as never[])); }
  count() { return [...this.listeners.values()].reduce((sum, handlers) => sum + handlers.size, 0); }
}
class Connection extends Emitter {
  peer = "";
  peerConnection = Object.assign(new EventTarget(), { getStats: vi.fn().mockResolvedValue(new Map<string, Record<string, unknown>>([
    ["transport", { type: "transport", selectedCandidatePairId: "pair" }],
    ["pair", { type: "candidate-pair", localCandidateId: "local", remoteCandidateId: "remote", currentRoundTripTime: 0.01 }],
    ["local", { candidateType: "relay", protocol: "udp", address: "SECRET", relayProtocol: "tcp" }],
    ["remote", { candidateType: "relay", protocol: "udp", address: "SECRET" }],
  ])) });
  send = vi.fn();
  close = vi.fn();
}
class TestPeer extends Emitter {
  connect = vi.fn();
  destroy = vi.fn();
}
let peers: TestPeer[];
let outgoing: Connection;
let incoming: Connection;
let controller: AbortController;
const credentials = vi.mocked(fetchFreshTurnConfig);
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("RTCPeerConnection", class {});
  peers = []; outgoing = new Connection(); incoming = new Connection(); controller = new AbortController();
  credentials.mockReset().mockResolvedValue({ iceServers: [{ urls: "turn:SECRET", username: "SECRET", credential: "SECRET" }] });
  mocks.create.mockReset().mockImplementation(() => { const peer = new TestPeer(); peer.connect.mockReturnValue(outgoing); peers.push(peer); return peer; });
});
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });
async function registered() {
  await vi.advanceTimersByTimeAsync(0);
  peers[0].emit("open"); peers[1].emit("open");
  await vi.advanceTimersByTimeAsync(0);
}
function attachIncoming() {
  incoming.peer = mocks.create.mock.calls[0][0];
  peers[1].emit("connection", incoming);
}
function assertClean() {
  for (const peer of peers) { expect(peer.destroy).toHaveBeenCalledTimes(1); expect(peer.count()).toBe(0); }
  expect(vi.getTimerCount()).toBe(0);
}
it("uses two explicit isolated relay-only PeerJS peers, real app options, echo and selected route evidence", async () => {
  const sources = getDiagnosticSources();
  const pending = runConnectivityDiagnostics(controller.signal);
  await registered(); attachIncoming();
  outgoing.send.mockImplementation((payload) => { incoming.emit("data", payload); });
  incoming.send.mockImplementation((payload) => { outgoing.emit("data", payload); });
  outgoing.emit("open");
  const results = await pending;
  expect(peers[0].connect).toHaveBeenCalledWith(mocks.create.mock.calls[1][0], PEER_CONNECT_OPTIONS);
  expect(mocks.create.mock.calls[0][0]).toMatch(/^phase-diagnostics-/);
  expect(mocks.create.mock.calls[0][0]).not.toBe(mocks.create.mock.calls[1][0]);
  for (const call of mocks.create.mock.calls) expect(call[1].config.iceTransportPolicy).toBe("relay");
  expect(results.map((result) => result.reason)).toEqual(["credentialsReady", "signalingReady", "relayReady"]);
  expect(results[2].evidence?.candidates).toMatchObject({ localType: "relay", remoteType: "relay", localProtocol: "udp", roundTripMs: 10 });
  expect(JSON.stringify(results)).not.toContain("SECRET");
  expect(getDiagnosticSources()).toEqual(sources);
  expect(outgoing.close).toHaveBeenCalledTimes(1); expect(incoming.close).toHaveBeenCalledTimes(1);
  assertClean();
});
it.each(["http", "network", "invalid", "no-turn"] as const)("reports %s credentials failure but independently checks signaling", async (reason) => {
  credentials.mockRejectedValue(new TurnCredentialError(reason, reason === "http" ? 503 : undefined));
  const pending = runConnectivityDiagnostics(controller.signal); await registered();
  const results = await pending;
  expect(results.map((result) => result.reason)).toEqual(["credentialsFailed", "signalingReady", "prerequisiteFailed"]);
  expect(results[0].evidence?.credentialFailure).toBe(reason);
  expect(peers[0].connect).not.toHaveBeenCalled();
  expect(mocks.create.mock.calls[0][1].config).toEqual({ iceServers: [], iceTransportPolicy: "relay" });
  assertClean();
});
it("bounds credential requests and aborts fetch before proceeding to signaling", async () => {
  credentials.mockImplementation(() => new Promise(() => {}));
  const pending = runConnectivityDiagnostics(controller.signal);
  await vi.advanceTimersByTimeAsync(8000);
  expect(credentials.mock.calls[0][0]?.aborted).toBe(true);
  await registered();
  expect((await pending)[0].reason).toBe("credentialsTimeout"); assertClean();
});
it.each(["error", "timeout", "constructor"])("cleans up signaling %s and never blames relay", async (failure) => {
  if (failure === "constructor") mocks.create.mockImplementationOnce(() => { const peer = new TestPeer(); peers.push(peer); return peer; }).mockImplementationOnce(() => { throw new Error("SECRET"); });
  const pending = runConnectivityDiagnostics(controller.signal);
  await vi.advanceTimersByTimeAsync(0);
  if (failure === "error") peers[0].emit("error", { type: "network", message: "SECRET" });
  if (failure === "timeout") await vi.advanceTimersByTimeAsync(10000);
  const results = await pending;
  expect(results[1].reason).toBe(failure === "timeout" ? "signalingTimeout" : "signalingFailed");
  expect(results[2]).toMatchObject({ status: "unavailable", reason: "prerequisiteFailed" });
  expect(JSON.stringify(results)).not.toContain("SECRET"); assertClean();
});
it.each(["credentials", "signaling", "relay"])("aborts during %s and disposes resources", async (stage) => {
  if (stage === "credentials") credentials.mockImplementation(() => new Promise(() => {}));
  const pending = expect(runConnectivityDiagnostics(controller.signal)).rejects.toMatchObject({ name: "AbortError" });
  if (stage === "relay") await registered(); else await vi.advanceTimersByTimeAsync(0);
  controller.abort(); await pending; assertClean();
  expect(credentials.mock.calls[0][0]?.aborted).toBe(true);
});
it.each([false, true])("never passes candidate-only connectivity (candidate=%s), retains bounded ICE codes", async (candidate) => {
  const pending = runConnectivityDiagnostics(controller.signal); await registered();
  if (candidate) outgoing.peerConnection.dispatchEvent(Object.assign(new Event("icecandidate"), { candidate: { type: "relay", address: "SECRET" } }));
  outgoing.peerConnection.dispatchEvent(Object.assign(new Event("icecandidateerror"), { errorCode: 701, url: "SECRET", errorText: "SECRET" }));
  await vi.advanceTimersByTimeAsync(15000);
  const results = await pending;
  expect(results[2]).toMatchObject({ status: "error", reason: candidate ? "relayNoEcho" : "relayNoCandidates", evidence: { iceErrorCodes: [701] } });
  expect(JSON.stringify(results)).not.toContain("SECRET"); assertClean();
});
it.each(["wrong", "send", "stats", "hangingStats"])("contains %s failures and cleans up without unhandled rejections", async (failure) => {
  const pending = runConnectivityDiagnostics(controller.signal); await registered();
  if (failure === "send") { outgoing.send.mockRejectedValue(new Error("SECRET")); outgoing.emit("open"); }
  else {
    if (failure === "stats") outgoing.peerConnection.getStats.mockRejectedValue(new Error("SECRET"));
    if (failure === "hangingStats") outgoing.peerConnection.getStats.mockImplementation(() => new Promise(() => {}));
    outgoing.emit("data", failure === "wrong" ? "wrong" : "phase-relay-check-v1");
  }
  await vi.advanceTimersByTimeAsync(15000);
  const results = await pending;
  expect(results[2].status).not.toBe("pass"); expect(JSON.stringify(results)).not.toContain("SECRET"); assertClean();
});
it("closes unexpected incoming peers and does not let them satisfy the echo", async () => {
  const pending = runConnectivityDiagnostics(controller.signal); await registered();
  const stranger = new Connection(); stranger.peer = "SECRET"; peers[1].emit("connection", stranger);
  expect(stranger.close).toHaveBeenCalledTimes(1);
  stranger.emit("data", "phase-relay-check-v1");
  await vi.advanceTimersByTimeAsync(15000);
  expect((await pending)[2].status).toBe("error"); assertClean();
});
it("returns unavailable without constructing peers in unsupported browsers", async () => {
  vi.stubGlobal("RTCPeerConnection", undefined);
  const results = await runConnectivityDiagnostics(controller.signal);
  expect(results.every((result) => result.status === "unavailable")).toBe(true);
  expect(credentials).not.toHaveBeenCalled(); expect(mocks.create).not.toHaveBeenCalled();
});

it("selects a genuine guest/host factory pair with one normalized host target", async () => {
  vi.resetModules();
  const transport = await import("../transport");
  const connection = await import("../connection");
  const diagnostics = await import("../connectivityDiagnostics");
  vi.mocked(connection.fetchFreshTurnConfig).mockResolvedValue({
    iceServers: [{ urls: "turn:SECRET", username: "SECRET", credential: "SECRET" }],
  });

  const contexts: Array<{ role: "guest" | "host"; hostPeerId: string }> = [];
  const creations: Array<{ role: "guest" | "host"; id?: string; options?: { config: RTCConfiguration } }> = [];
  const factoryFor = (role: "guest" | "host") => ({
    create(id?: string, options?: { config: RTCConfiguration }) {
      creations.push({ role, id, options });
      const peer = new TestPeer();
      peer.connect.mockReturnValue(outgoing);
      peers.push(peer);
      return peer as never;
    },
  });
  const guestFactory = factoryFor("guest");
  const hostFactory = factoryFor("host");
  transport.installPeerTransportSelector((context) => {
    contexts.push(context);
    return context.role === "guest" ? guestFactory : hostFactory;
  });

  const pending = diagnostics.runConnectivityDiagnostics(controller.signal);
  await registered();
  incoming.peer = creations[0].id!;
  peers[1].emit("connection", incoming);
  outgoing.send.mockImplementation((payload) => { incoming.emit("data", payload); });
  incoming.send.mockImplementation((payload) => { outgoing.emit("data", payload); });
  outgoing.emit("open");
  const results = await pending;

  expect(creations.map(({ role }) => role)).toEqual(["guest", "host"]);
  expect(creations[0].id).toMatch(/^phase-diagnostics-/);
  expect(creations[1].id).toMatch(/^phase-diagnostics-/);
  expect(creations[0].id).not.toBe(creations[1].id);
  expect(contexts).toEqual([
    { role: "guest", hostPeerId: creations[1].id },
    { role: "host", hostPeerId: creations[1].id },
  ]);
  expect(creations.map(({ options }) => options?.config.iceTransportPolicy)).toEqual(["relay", "relay"]);
  expect(peers[0].connect).toHaveBeenCalledWith(creations[1].id, PEER_CONNECT_OPTIONS);
  expect(results.map((result) => result.reason)).toEqual(["credentialsReady", "signalingReady", "relayReady"]);
  expect(mocks.create).not.toHaveBeenCalled();
  assertClean();
});

it("contains a selector failure as signaling failure and cleans an earlier selected peer", async () => {
  vi.resetModules();
  const transport = await import("../transport");
  const connection = await import("../connection");
  const diagnostics = await import("../connectivityDiagnostics");
  vi.mocked(connection.fetchFreshTurnConfig).mockResolvedValue({
    iceServers: [{ urls: "turn:SECRET", username: "SECRET", credential: "SECRET" }],
  });
  const factory = {
    create: vi.fn(() => {
      const peer = new TestPeer();
      peer.connect.mockReturnValue(outgoing);
      peers.push(peer);
      return peer as never;
    }),
  };
  const selector = vi.fn((context: { role: "guest" | "host" }) => {
    if (context.role === "host") throw new Error("timeout");
    return factory;
  });
  transport.installPeerTransportSelector(selector);

  const pending = diagnostics.runConnectivityDiagnostics(controller.signal);
  await vi.advanceTimersByTimeAsync(0);
  const results = await pending;

  expect(selector).toHaveBeenCalledTimes(2);
  expect(factory.create).toHaveBeenCalledTimes(1);
  expect(results.map(({ reason }) => reason)).toEqual(["credentialsReady", "signalingFailed", "prerequisiteFailed"]);
  expect(results[1].evidence?.peerError).toBe("unknown");
  expect(JSON.stringify(results)).not.toContain("SECRET");
  expect(mocks.create).not.toHaveBeenCalled();
  assertClean();
});

it("contains a second selected-factory construction failure and destroys the first peer", async () => {
  vi.resetModules();
  const transport = await import("../transport");
  const connection = await import("../connection");
  const diagnostics = await import("../connectivityDiagnostics");
  vi.mocked(connection.fetchFreshTurnConfig).mockResolvedValue({
    iceServers: [{ urls: "turn:SECRET", username: "SECRET", credential: "SECRET" }],
  });
  const factory = {
    create: vi.fn()
      .mockImplementationOnce(() => {
        const peer = new TestPeer();
        peer.connect.mockReturnValue(outgoing);
        peers.push(peer);
        return peer as never;
      })
      .mockImplementationOnce(() => { throw new Error("SECRET constructor failure"); }),
  };
  const selector = vi.fn(() => factory);
  transport.installPeerTransportSelector(selector);

  const pending = diagnostics.runConnectivityDiagnostics(controller.signal);
  await vi.advanceTimersByTimeAsync(0);
  const results = await pending;

  expect(selector).toHaveBeenCalledTimes(2);
  expect(factory.create).toHaveBeenCalledTimes(2);
  expect(peers[0].destroy).toHaveBeenCalledTimes(1);
  expect(results.map(({ reason }) => reason)).toEqual(["credentialsReady", "signalingFailed", "prerequisiteFailed"]);
  expect(results[1].evidence?.peerError).toBe("unknown");
  expect(JSON.stringify(results)).not.toContain("SECRET");
  expect(mocks.create).not.toHaveBeenCalled();
  assertClean();
});

it("does not install or lock a transport selector for unsupported diagnostics", async () => {
  vi.resetModules();
  const transport = await import("../transport");
  const diagnostics = await import("../connectivityDiagnostics");
  vi.stubGlobal("RTCPeerConnection", undefined);

  const results = await diagnostics.runConnectivityDiagnostics(controller.signal);
  const selector = vi.fn(() => transport.peerTransportFactory);

  expect(results.every((result) => result.status === "unavailable")).toBe(true);
  expect(() => transport.installPeerTransportSelector(selector)).not.toThrow();
  expect(selector).not.toHaveBeenCalled();
  expect(mocks.create).not.toHaveBeenCalled();
});

it.each(["network", "webrtc"])("distinguishes %s peer errors during relay from signaling outages", async (type) => {
  const pending = runConnectivityDiagnostics(controller.signal); await registered();
  peers[0].emit("error", { type, message: "SECRET" });
  const results = await pending;
  expect(results[1].status).toBe(type === "network" ? "error" : "pass");
  expect(results[2].status).toBe(type === "network" ? "unavailable" : "error");
  assertClean();
});
it("accepts selected local relay with remote peer-reflexive evidence", async () => {
  const pending = runConnectivityDiagnostics(controller.signal); await registered();
  outgoing.peerConnection.getStats.mockResolvedValue(new Map([
    ["transport", { type: "transport", selectedCandidatePairId: "pair" }],
    ["pair", { type: "candidate-pair", localCandidateId: "local", remoteCandidateId: "remote" }],
    ["local", { candidateType: "relay", protocol: "udp" }],
    ["remote", { candidateType: "prflx", protocol: "udp" }],
  ]));
  outgoing.emit("data", "phase-relay-check-v1");
  expect((await pending)[2].reason).toBe("relayReady"); assertClean();
});
it("keeps echoed but hanging statistics unavailable", async () => {
  const pending = runConnectivityDiagnostics(controller.signal); await registered();
  outgoing.peerConnection.getStats.mockImplementation(() => new Promise(() => {}));
  outgoing.emit("data", "phase-relay-check-v1");
  await vi.advanceTimersByTimeAsync(15000);
  expect((await pending)[2]).toMatchObject({ status: "unavailable", reason: "relayEvidenceUnavailable" }); assertClean();
});
it("suppresses a deferred stats call when cancellation already closed its resources", async () => {
  const pending = expect(runConnectivityDiagnostics(controller.signal)).rejects.toMatchObject({ name: "AbortError" }); await registered();
  outgoing.emit("data", "phase-relay-check-v1");
  controller.abort();
  await pending;
  expect(outgoing.peerConnection.getStats).not.toHaveBeenCalled();
  assertClean();
});

it.each(["negotiation-failed", "connection-closed", "SECRET"])("retains bounded DataConnection failure %s before teardown", async (type) => {
  const pending = runConnectivityDiagnostics(controller.signal); await registered();
  Object.assign(outgoing.peerConnection, { connectionState: "failed", iceConnectionState: "failed" });
  outgoing.close.mockImplementation(() => { Object.assign(outgoing.peerConnection, { connectionState: "closed", iceConnectionState: "closed" }); });
  outgoing.emit("error", { type, message: "SECRET" });
  const results = await pending;
  expect(results[2]).toMatchObject({ status: "error", reason: "relayConnectionFailed", evidence: {
    connectionError: type === "SECRET" ? "unknown" : type,
    connectionState: { connectionState: "failed", iceState: "failed", channelState: null },
  } });
  expect(JSON.stringify(results)).not.toContain("SECRET");
  assertClean();
});
