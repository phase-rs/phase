import { DIAGNOSTIC_HISTORY_LIMIT, isRetainedDiagnosticEntry, loadDiagnosticHistory, saveDiagnosticHistory } from "./diagnosticHistory";

/** Local, identity-free observations. Sources are local instances, not gameplay ownership. */
export interface EngineDiagnosticSnapshot {
  observedAt: number;
  initialized: boolean;
  initializing: boolean;
  disposed: boolean;
  execution: "worker" | "main-thread" | "unavailable";
}

export type DisconnectCause = "send-error" | "connection-close" | "connection-error" | "remote-disconnect" | "local-close";
export interface TransportDiagnosticSnapshot {
  observedAt: number;
  connectionState: RTCPeerConnectionState | null;
  iceState: RTCIceConnectionState | null;
  channelState: RTCDataChannelState | null;
  bufferedBytes: number | null;
  pendingSends: number;
  pendingDecodes: number;
  receiveAgeMs: number | null;
  pongAgeMs: number | null;
  channelError: "data-channel-error" | null;
}
export interface CandidateDiagnosticSnapshot {
  observedAt?: number;
  localProtocol?: "udp" | "tcp" | null;
  relayProtocol?: "udp" | "tcp" | "tls" | null;
  remoteProtocol?: "udp" | "tcp" | null;
  localType: RTCIceCandidateType | null;
  remoteType: RTCIceCandidateType | null;
  roundTripMs: number | null;
}
export interface EngineDiagnosticSource {
  snapshot(): EngineDiagnosticSnapshot;
  ping(): Promise<unknown>;
}
export interface PeerDiagnosticSource {
  diagnosticId?: string;
  snapshot(): TransportDiagnosticSnapshot;
  stats(): Promise<CandidateDiagnosticSnapshot | null>;
}
export type TurnCredentialFailure = "http" | "network" | "invalid" | "no-turn" | "aborted";
export type PeerDiagnosticError = "browser-incompatible" | "disconnected" | "invalid-id" | "invalid-key" | "network" | "peer-unavailable" | "ssl-unavailable" | "server-error" | "socket-error" | "socket-closed" | "unavailable-id" | "webrtc" | "unknown";
export type ConnectionDiagnosticError = "negotiation-failed" | "connection-closed" | "message-too-big" | "unknown";
export interface ConnectionFailureSnapshot {
  connectionState: RTCPeerConnectionState | null;
  iceState: RTCIceConnectionState | null;
  channelState: RTCDataChannelState | null;
}
export type DiagnosticHistoryEntry = ({ diagnosticId?: string; peerDiagnosticId?: string } & (
  | { kind: "signaling"; observedAt: number; side: "Host" | "Guest"; event: "created" | "open" | "disconnected" | "close" | "timeout" | "error" | "constructor-error"; error?: PeerDiagnosticError }
  | { kind: "credentials"; observedAt: number; outcome: "fresh" | "cache" | "stun-fallback"; failure?: TurnCredentialFailure; httpStatus?: number }
  | { kind: "connection-attempt"; observedAt: number; direction: "incoming" | "outgoing"; error?: ConnectionDiagnosticError; state?: ConnectionFailureSnapshot; event: "started" | "open" | "close" | "error" | "timeout" | "aborted" }
  | { kind: "ice-candidate-error"; observedAt: number; code: number }
  | { kind: "candidate-route"; observedAt: number; candidates: CandidateDiagnosticSnapshot }
  | { kind: "disconnect"; observedAt: number; cause: DisconnectCause; error?: ConnectionDiagnosticError; transport: TransportDiagnosticSnapshot; preClose: TransportDiagnosticSnapshot | null; candidates?: CandidateDiagnosticSnapshot | null }
  | { kind: "engine-not-initialized"; observedAt: number; operation: string; engine: EngineDiagnosticSnapshot }));

const engines = new Set<EngineDiagnosticSource>();
const peers = new Set<PeerDiagnosticSource>();
const history: DiagnosticHistoryEntry[] = loadDiagnosticHistory();
const diagnosticIds = new WeakMap<object, string>();
/** Random diagnostic identity is unrelated to PeerJS IDs, room codes or players. */
export function diagnosticIdFor(source: object): string {
  let id = diagnosticIds.get(source);
  if (!id) { id = crypto.randomUUID(); diagnosticIds.set(source, id); }
  return id;
}
const SOURCE_LIMIT = 16;
export { DIAGNOSTIC_HISTORY_LIMIT } from "./diagnosticHistory";

export function registerEngineDiagnostics(source: EngineDiagnosticSource): () => void {
  if (engines.size < SOURCE_LIMIT) engines.add(source);
  return () => { engines.delete(source); };
}
export function registerPeerDiagnostics(source: PeerDiagnosticSource): () => void {
  if (peers.size < SOURCE_LIMIT) peers.add(source);
  return () => { peers.delete(source); };
}
export function getDiagnosticSources(): { engines: EngineDiagnosticSource[]; peers: PeerDiagnosticSource[] } {
  return { engines: [...engines], peers: [...peers] };
}
export function recordDiagnostic(entry: DiagnosticHistoryEntry): void {
  if (!isRetainedDiagnosticEntry(entry)) return;
  history.push(structuredClone(entry));
  if (history.length > DIAGNOSTIC_HISTORY_LIMIT) history.shift();
  saveDiagnosticHistory(history);
}
export function getDiagnosticHistory(): DiagnosticHistoryEntry[] {
  const retained = history.filter(isRetainedDiagnosticEntry);
  history.splice(0, history.length, ...retained);
  saveDiagnosticHistory(history);
  return structuredClone(history);
}

/** Only expose selected candidate kinds and measured RTT; never return RTCStatsReport. */
export function projectCandidateStats(report: RTCStatsReport): CandidateDiagnosticSnapshot | null {
  let pair: RTCIceCandidatePairStats | undefined;
  report.forEach((stat) => {
    if (stat.type === "transport" && stat.selectedCandidatePairId) pair = report.get(stat.selectedCandidatePairId);
  });
  if (!pair) report.forEach((stat) => {
    if (stat.type === "candidate-pair" && stat.nominated && stat.state === "succeeded") pair = stat;
  });
  if (!pair) return null;
  const candidateType = (id: string | undefined): RTCIceCandidateType | null => {
    const type: unknown = id ? report.get(id)?.candidateType : null;
    return type === "host" || type === "srflx" || type === "prflx" || type === "relay" ? type : null;
  };
  const protocol = (id: string | undefined): "udp" | "tcp" | null => {
    const value: unknown = id ? report.get(id)?.protocol : null;
    return value === "udp" || value === "tcp" ? value : null;
  };
  const relayProtocol: unknown = pair.localCandidateId ? report.get(pair.localCandidateId)?.relayProtocol : null;
  return {
    observedAt: Date.now(),
    relayProtocol: candidateType(pair.localCandidateId) === "relay" && (relayProtocol === "udp" || relayProtocol === "tcp" || relayProtocol === "tls") ? relayProtocol : null,
    localProtocol: protocol(pair.localCandidateId),
    remoteProtocol: protocol(pair.remoteCandidateId),
    localType: candidateType(pair.localCandidateId),
    remoteType: candidateType(pair.remoteCandidateId),
    roundTripMs: typeof pair.currentRoundTripTime === "number" && Number.isFinite(pair.currentRoundTripTime)
      ? Math.max(0, pair.currentRoundTripTime * 1000) : null,
  };
}

/** Bounds a read-only probe, without starting or recovering an engine. */
class DiagnosticTimeout extends Error {
  constructor() { super("Diagnostic timeout"); }
}

export async function boundedDiagnosticProbe<T>(probe: () => Promise<T>, timeoutMs = 1500, signal?: AbortSignal): Promise<T> {
  signal?.throwIfAborted();
  let abort: (() => void) | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      Promise.resolve().then(() => { signal?.throwIfAborted(); return probe(); }),
      ...(signal ? [new Promise<never>((_, reject) => {
        abort = () => reject(signal.reason);
        signal.addEventListener("abort", abort, { once: true });
      })] : []),
      new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new DiagnosticTimeout()), timeoutMs); }),
    ]);
  } finally {
    clearTimeout(timer);
    if (signal && abort) signal.removeEventListener("abort", abort);
  }
}

export type DiagnosticStatus = "pass" | "warning" | "unavailable" | "error";
export type DiagnosticReason = "networkUnsupported" | "credentialsReady" | "credentialsFailed" | "credentialsTimeout" | "prerequisiteFailed" | "signalingReady" | "signalingFailed" | "signalingTimeout" | "relayReady" | "relayEvidenceUnavailable" | "relayNoCandidates" | "relayNoEcho" | "online" | "offline" | "visible" | "hidden" | "supported" | "unsupported" | "noEngine" | "engineReady" | "engineWaiting" | "probeFailed" | "probeTimeout" | "noPeer" | "peerConnected" | "peerDisconnectedDuringCheck" | "relayConnectionFailed" | "peerWaiting" | "peerFailed" | "relayed" | "direct" | "statsUnavailable" | "directoryReached" | "directoryFailed" | "modeNotChecked" | "draftModeNotChecked";
export interface DiagnosticProbeEvidence {
  durationMs?: number;
  credentialFailure?: TurnCredentialFailure;
  httpStatus?: number;
  peerError?: PeerDiagnosticError;
  connectionError?: ConnectionDiagnosticError;
  connectionState?: ConnectionFailureSnapshot;
  iceErrorCodes?: number[];
  relayCandidates?: number;
  candidates?: CandidateDiagnosticSnapshot | null;
}
export type ConnectivityDiagnosticProbe = (signal: AbortSignal) => Promise<DiagnosticResult[]>;
export interface DiagnosticResult {
  diagnosticId?: string;
  check: "credentials" | "signaling" | "relay" | "connectivity" | "visibility" | "support" | "engine" | "peer" | "route" | "directory" | "mode";
  status: DiagnosticStatus;
  reason: DiagnosticReason;
  observedAt: number;
  evidence?: DiagnosticProbeEvidence;
}
export interface DiagnosticEnvironment {
  version: string;
  build: string;
  mode: "ai" | "native-ai" | "online" | "local" | "p2p-host" | "p2p-join" | "draft-match" | "spectate" | null;
  online: boolean;
  visibility: DocumentVisibilityState;
  webAssembly: boolean;
  webRtc: boolean;
}
export interface DiagnosticReport {
  schemaVersion: 1;
  startedAt: number;
  completedAt: number;
  environment: DiagnosticEnvironment;
  results: DiagnosticResult[];
  engines: EngineDiagnosticSnapshot[];
  peers: { diagnosticId: string; transport: TransportDiagnosticSnapshot; candidates: CandidateDiagnosticSnapshot | null }[];
  history: DiagnosticHistoryEntry[];
}

/** Local observations run alongside an explicitly supplied isolated connectivity probe. */
export async function runDiagnostics(environment: DiagnosticEnvironment, endpoint: string, signal: AbortSignal, connectivityProbe?: ConnectivityDiagnosticProbe): Promise<DiagnosticReport> {
  signal.throwIfAborted();
  const startedAt = Date.now();
  const sources = getDiagnosticSources();
  const results: DiagnosticResult[] = [];
  const engineSnapshots: EngineDiagnosticSnapshot[] = [];
  const peerSnapshots: DiagnosticReport["peers"] = [];
  const add = (check: DiagnosticResult["check"], status: DiagnosticStatus, reason: DiagnosticReason, diagnosticId?: string, evidence?: DiagnosticProbeEvidence) => {
    signal.throwIfAborted();
    results.push({ check, status, reason, observedAt: Date.now(), ...(diagnosticId ? { diagnosticId } : {}), ...(evidence ? { evidence } : {}) });
  };
  add("connectivity", environment.online ? "pass" : "warning", environment.online ? "online" : "offline");
  add("visibility", environment.visibility === "visible" ? "pass" : "warning", environment.visibility === "visible" ? "visible" : "hidden");
  add("support", environment.webAssembly && environment.webRtc ? "pass" : "unavailable", environment.webAssembly && environment.webRtc ? "supported" : "unsupported");
  // Registry entries can be auxiliary instances; they do not establish gameplay ownership.
  if (environment.mode === "online" || environment.mode === "native-ai" || environment.mode === "spectate") add("mode", "unavailable", "modeNotChecked");
  if (environment.mode === "draft-match") add("mode", "unavailable", "draftModeNotChecked");
  if (!sources.engines.length) add("engine", "unavailable", "noEngine");
  const observePeer = async (source: PeerDiagnosticSource) => {
      const diagnosticId = source.diagnosticId ?? diagnosticIdFor(source);
      try {
        const transport = source.snapshot();
        const peer = { diagnosticId, transport, candidates: null as CandidateDiagnosticSnapshot | null };
        peerSnapshots.push(peer);
        try {
          peer.candidates = await boundedDiagnosticProbe(() => source.stats(), 1500, signal);
          if (!peer.candidates) add("route", "unavailable", "statsUnavailable", diagnosticId);
          else if (peer.candidates.localType === "relay" || peer.candidates.remoteType === "relay") add("route", "pass", "relayed", diagnosticId, { candidates: peer.candidates });
          else if (peer.candidates.localType && peer.candidates.remoteType) add("route", "pass", "direct", diagnosticId, { candidates: peer.candidates });
          else add("route", "unavailable", "statsUnavailable", diagnosticId);
        } catch { add("route", "unavailable", "statsUnavailable", diagnosticId); }
      } catch { add("peer", "unavailable", "probeFailed", diagnosticId); }
  };
  await Promise.all([
    ...(connectivityProbe ? [(async () => {
      const connectivity = await connectivityProbe(signal);
      signal.throwIfAborted();
      results.unshift(...connectivity);
    })()] : []),
    ...sources.engines.map(async (source) => {
      try {
        const snapshot = source.snapshot();
        engineSnapshots.push(snapshot);
        if (!snapshot.initialized || snapshot.disposed) {
          add("engine", "unavailable", "engineWaiting");
          return;
        }
        await boundedDiagnosticProbe(() => source.ping(), 1500, signal);
        add("engine", "pass", "engineReady");
      } catch (error) {
        add("engine", "warning", error instanceof DiagnosticTimeout ? "probeTimeout" : "probeFailed");
      }
    }),
    ...sources.peers.map(observePeer),
    (async () => {
      const controller = new AbortController();
      const abort = () => controller.abort();
      signal.addEventListener("abort", abort, { once: true });
      try {
        const response = await boundedDiagnosticProbe(() => fetch(endpoint, {
          method: "GET", signal: controller.signal, cache: "no-store", credentials: "omit", referrerPolicy: "no-referrer",
        }), 2500, signal);
        // HTTP reachability only. Do not ingest directory bodies, server IDs or addresses.
        void response.body?.cancel().catch(() => {});
        add("directory", response.ok ? "pass" : "warning", response.ok ? "directoryReached" : "directoryFailed");
      } catch { add("directory", "warning", "directoryFailed"); }
      finally { controller.abort(); signal.removeEventListener("abort", abort); }
    })(),
  ]);
  signal.throwIfAborted();
  const addedPeers = getDiagnosticSources().peers.filter((source) => !sources.peers.includes(source));
  await Promise.all(addedPeers.map(observePeer));
  signal.throwIfAborted();
  sources.peers.push(...addedPeers);
  if (!sources.peers.length) add("peer", "unavailable", "noPeer");
  for (const source of sources.peers) {
    const diagnosticId = source.diagnosticId ?? diagnosticIdFor(source);
    const peer = peerSnapshots.find((snapshot) => snapshot.diagnosticId === diagnosticId);
    if (!peer) continue;
    try {
      const wasConnected = peer.transport.connectionState === "connected" && peer.transport.channelState === "open";
      peer.transport = source.snapshot();
      const transport = peer.transport;
      const registered = peers.has(source);
      const connected = registered && transport.connectionState === "connected" && transport.channelState === "open";
      const failed = transport.connectionState === "failed" || transport.iceState === "failed" || transport.channelError !== null;
      add("peer", failed ? "error" : connected ? "pass" : "warning", failed ? "peerFailed" : wasConnected && !connected ? "peerDisconnectedDuringCheck" : connected ? "peerConnected" : "peerWaiting", diagnosticId);
    } catch { add("peer", "unavailable", "probeFailed", diagnosticId); }
  }
  return { schemaVersion: 1, startedAt, completedAt: Date.now(), environment: { ...environment }, results, engines: engineSnapshots, peers: peerSnapshots, history: getDiagnosticHistory() };
}
