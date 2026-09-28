import { connectionFailureSnapshot, fetchFreshTurnConfig, PEER_CONNECT_OPTIONS, safeConnectionError, safePeerError, TurnCredentialError } from "./connection";
import { createPeer } from "./transport";
import type { TransportConnection, TransportPeer } from "./transport";
import { boundedDiagnosticProbe, projectCandidateStats, type DiagnosticProbeEvidence, type DiagnosticResult } from "../services/troubleshooting";

const CHALLENGE = "phase-relay-check-v1";
const CREDENTIAL_TIMEOUT_MS = 8_000;
const SIGNALING_TIMEOUT_MS = 10_000;
const RELAY_TIMEOUT_MS = 15_000;

/** An isolated PeerJS/BinaryPack round trip, using the game's signaling defaults. */
export async function runConnectivityDiagnostics(signal: AbortSignal): Promise<DiagnosticResult[]> {
  signal.throwIfAborted();
  const results: DiagnosticResult[] = [];
  const add = (check: "credentials" | "signaling" | "relay", status: DiagnosticResult["status"], reason: DiagnosticResult["reason"], startedAt: number, evidence?: DiagnosticResult["evidence"]) => {
    results.push({ check, status, reason, observedAt: Date.now(), evidence: { durationMs: Date.now() - startedAt, ...evidence } });
  };
  if (typeof RTCPeerConnection === "undefined" || typeof globalThis.crypto?.randomUUID !== "function") {
    for (const check of ["credentials", "signaling", "relay"] as const) add(check, "unavailable", "networkUnsupported", Date.now());
    return results;
  }
  const credentialsStarted = Date.now();
  const controller = new AbortController();
  const abortFetch = () => controller.abort();
  signal.addEventListener("abort", abortFetch, { once: true });
  let credentialTimeout = false;
  const credentialTimer = setTimeout(() => { credentialTimeout = true; controller.abort(); }, CREDENTIAL_TIMEOUT_MS);
  let config: RTCConfiguration = { iceServers: [] };
  let credentialsReady = false;
  try {
    config = await boundedDiagnosticProbe(() => fetchFreshTurnConfig(controller.signal), CREDENTIAL_TIMEOUT_MS, signal);
    credentialsReady = true;
    add("credentials", "pass", "credentialsReady", credentialsStarted);
  } catch (error) {
    signal.throwIfAborted();
    add("credentials", "error", credentialTimeout ? "credentialsTimeout" : "credentialsFailed", credentialsStarted,
      !credentialTimeout && error instanceof TurnCredentialError ? { credentialFailure: error.reason, httpStatus: error.httpStatus } : undefined);
  } finally {
    clearTimeout(credentialTimer);
    controller.abort();
    signal.removeEventListener("abort", abortFetch);
  }
  signal.throwIfAborted();
  const peers: TransportPeer[] = [];
  const connections: TransportConnection[] = [];
  const removeListeners: (() => void)[] = [];
  const iceErrorCodes = new Set<number>();
  let relayCandidates = 0;
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let failStage: (error: unknown) => void = () => {};
  const onAbort = () => failStage(signal.reason);
  signal.addEventListener("abort", onAbort, { once: true });
  const closeConnection = (connection: TransportConnection) => { try { connection.close(); } catch { /* Continue disposing the remaining resources. */ } };
  const stage = <T>(timeoutMs: number, start: (resolve: (value: T) => void, reject: (error: unknown) => void) => void): Promise<T> => new Promise((resolve, reject) => {
    let finished = false;
    const finish = (done: () => void) => { if (finished || stopped) return; finished = true; clearTimeout(timer); done(); };
    failStage = (error) => finish(() => reject(error));
    timer = setTimeout(() => failStage(new Error("timeout")), timeoutMs);
    try { signal.throwIfAborted(); start((value) => finish(() => resolve(value)), failStage); }
    catch (error) { failStage(error); }
  });
  let relayPeerError: ReturnType<typeof safePeerError> | undefined;
  let connectionFailure: DiagnosticProbeEvidence = {};
  const evidence = () => ({ ...connectionFailure, iceErrorCodes: [...iceErrorCodes], relayCandidates, ...(relayPeerError ? { peerError: relayPeerError } : {}) });
  const rejectIncoming = (connection: TransportConnection) => closeConnection(connection);
  let signalingFailure: ReturnType<typeof safePeerError> | undefined;
  try {
    const ids = [crypto.randomUUID(), crypto.randomUUID()].map((id) => `phase-diagnostics-${id}`);
    const signalingStarted = Date.now();
    let registered = 0;
    try {
      await stage<void>(SIGNALING_TIMEOUT_MS, (resolve, reject) => {
        for (const id of ids) {
          const peer = createPeer(id, { config: { ...config, iceTransportPolicy: "relay" } });
          peers.push(peer);
          const onError = (error: unknown) => {
            const type = safePeerError(error);
            if (registered < 2 || type === "network" || type === "disconnected" || type === "socket-error" || type === "socket-closed" || type === "server-error" || type === "ssl-unavailable") signalingFailure = type;
            else relayPeerError = type;
            failStage(error);
          };
          const onClosed = () => { signalingFailure = "disconnected"; failStage(new Error("closed")); };
          const onOpen = () => { registered += 1; if (registered === 2) resolve(); };
          peer.once("open", onOpen);
          peer.on("error", onError);
          peer.on("disconnected", onClosed);
          peer.on("close", onClosed);
          peer.on("connection", rejectIncoming);
          removeListeners.push(() => {
            peer.off("open", onOpen); peer.off("error", onError); peer.off("disconnected", onClosed); peer.off("close", onClosed); peer.off("connection", rejectIncoming);
          });
          if (signal.aborted) { reject(signal.reason); break; }
        }
      });
      add("signaling", "pass", "signalingReady", signalingStarted);
    } catch (error) {
      signal.throwIfAborted();
      add("signaling", "error", registered < 2 && !signalingFailure && error instanceof Error && error.message === "timeout" ? "signalingTimeout" : "signalingFailed", signalingStarted, { peerError: signalingFailure ?? safePeerError(error) });
      add("relay", "unavailable", "prerequisiteFailed", Date.now());
      return results;
    }
    if (!credentialsReady) {
      add("relay", "unavailable", "prerequisiteFailed", Date.now());
      return results;
    }
    const relayStarted = Date.now();
    let echoed = false;
    const route: { candidates: ReturnType<typeof projectCandidateStats> } = { candidates: null };
    try {
      await stage<void>(RELAY_TIMEOUT_MS, (resolve, reject) => {
        const observe = (connection: TransportConnection) => {
          connections.push(connection);
          const onError = (error: unknown) => {
            connectionFailure = { connectionError: safeConnectionError(error), connectionState: connectionFailureSnapshot(connection) };
            reject(new Error("connection"));
          };
          const onClose = () => onError({ type: "connection-closed" });
          connection.on("error", onError);
          connection.on("close", onClose);
          removeListeners.push(() => { connection.off("error", onError); connection.off("close", onClose); });
          const pc = connection.peerConnection;
          const onCandidate = (event: RTCPeerConnectionIceEvent) => { if (event.candidate?.type === "relay") relayCandidates = Math.min(16, relayCandidates + 1); };
          const onIceError = (event: RTCPeerConnectionIceErrorEvent) => {
            if (iceErrorCodes.size < 8 && Number.isInteger(event.errorCode) && event.errorCode >= 300 && event.errorCode <= 799) iceErrorCodes.add(event.errorCode);
          };
          pc?.addEventListener("icecandidate", onCandidate);
          pc?.addEventListener("icecandidateerror", onIceError);
          removeListeners.push(() => { pc?.removeEventListener("icecandidate", onCandidate); pc?.removeEventListener("icecandidateerror", onIceError); });
        };
        const send = (connection: TransportConnection) => {
          try { void Promise.resolve(connection.send(CHALLENGE)).catch(reject); }
          catch (error) { reject(error); }
        };
        const onIncoming = (connection: TransportConnection) => {
          if (stopped || connection.peer !== ids[0] || connections.length > 1) { closeConnection(connection); return; }
          observe(connection);
          let replied = false;
          const onData = (data: unknown) => {
            if (replied || stopped) return;
            if (data !== CHALLENGE) { reject(new Error("payload")); return; }
            replied = true;
            send(connection);
          };
          connection.on("data", onData);
          removeListeners.push(() => connection.off("data", onData));
        };
        peers[1].off("connection", rejectIncoming);
        peers[1].on("connection", onIncoming);
        removeListeners.push(() => peers[1].off("connection", onIncoming));
        const outgoing = peers[0].connect(ids[1], PEER_CONNECT_OPTIONS);
        observe(outgoing);
        const onOpen = () => send(outgoing);
        const onData = (data: unknown) => {
          if (echoed || stopped) return;
          if (data !== CHALLENGE) { reject(new Error("payload")); return; }
          echoed = true;
          // Only selected-pair evidence can certify the relay round trip.
          const peerConnection = outgoing.peerConnection;
          void Promise.resolve().then(() => {
            if (stopped || signal.aborted || !peerConnection || peerConnection.signalingState === "closed") return null;
            return peerConnection.getStats();
          }).then((stats) => {
            if (stopped) return;
            route.candidates = stats ? projectCandidateStats(stats) : null;
            resolve();
          }).catch(() => resolve());
        };
        outgoing.once("open", onOpen);
        outgoing.on("data", onData);
        removeListeners.push(() => { outgoing.off("open", onOpen); outgoing.off("data", onData); });
      });
      const relayed = route.candidates?.localType === "relay";
      add("relay", relayed ? "pass" : "unavailable", relayed ? "relayReady" : "relayEvidenceUnavailable", relayStarted, { ...evidence(), candidates: route.candidates });
    } catch {
      signal.throwIfAborted();
      if (signalingFailure) {
        results[1] = { ...results[1], status: "error", reason: "signalingFailed", observedAt: Date.now(), evidence: { ...results[1].evidence, peerError: signalingFailure } };
        add("relay", "unavailable", "prerequisiteFailed", relayStarted, evidence());
      } else add("relay", echoed ? "unavailable" : "error", echoed ? "relayEvidenceUnavailable" : connectionFailure.connectionError ? "relayConnectionFailed" : relayCandidates === 0 ? "relayNoCandidates" : "relayNoEcho", relayStarted, evidence());
    }
    return results;
  } finally {
    stopped = true;
    clearTimeout(timer);
    signal.removeEventListener("abort", onAbort);
    removeListeners.reverse().forEach((remove) => remove());
    connections.forEach(closeConnection);
    peers.forEach((peer) => { try { peer.destroy(); } catch { /* Continue disposing the other peer. */ } });
  }
}
