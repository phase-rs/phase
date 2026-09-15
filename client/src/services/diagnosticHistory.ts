import type { DiagnosticHistoryEntry } from "./troubleshooting";

const STORAGE_KEY = "phase-diagnostic-history-v1";
export const DIAGNOSTIC_HISTORY_MAX_AGE_MS = 60 * 60 * 1000;
export const DIAGNOSTIC_HISTORY_LIMIT = 30;
const MAX_BYTES = 64 * 1024;
type Check = (value: unknown) => boolean;
const number: Check = (value) => typeof value === "number" && Number.isFinite(value) && value >= 0;
const boolean: Check = (value) => typeof value === "boolean";
const oneOf = (...values: unknown[]): Check => (value) => values.includes(value);
const nullable = (check: Check): Check => (value) => value === null || check(value);
const optional = (check: Check): Check => (value) => value === undefined || check(value);
const identity: Check = (value) => typeof value === "string" && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);
const connectionState = nullable(oneOf("new", "connecting", "connected", "disconnected", "failed", "closed"));
const iceState = nullable(oneOf("new", "checking", "connected", "completed", "disconnected", "failed", "closed"));
const channelState = nullable(oneOf("connecting", "open", "closing", "closed"));
const failureState = { connectionState, iceState, channelState };
const shape = (fields: Record<string, Check>): Check => (value) => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  return Object.entries(value).every(([key, item]) => Object.prototype.hasOwnProperty.call(fields, key) && fields[key](item))
    && Object.entries(fields).every(([key, check]) => check(Reflect.get(value, key)));
};
const transport = shape({
  observedAt: number, ...failureState, bufferedBytes: nullable(number), pendingSends: number,
  pendingDecodes: number, receiveAgeMs: nullable(number), pongAgeMs: nullable(number),
  channelError: oneOf(null, "data-channel-error"),
});
const candidates = shape({
  observedAt: optional(number), localProtocol: optional(oneOf(null, "udp", "tcp")),
  remoteProtocol: optional(oneOf(null, "udp", "tcp")), relayProtocol: optional(oneOf(null, "udp", "tcp", "tls")),
  localType: oneOf(null, "host", "srflx", "prflx", "relay"), remoteType: oneOf(null, "host", "srflx", "prflx", "relay"),
  roundTripMs: nullable(number),
});
const engine = shape({ observedAt: number, initialized: boolean, initializing: boolean, disposed: boolean, execution: oneOf("worker", "main-thread", "unavailable") });
const common = { observedAt: number, diagnosticId: optional(identity), peerDiagnosticId: optional(identity) };
const schemas: Record<DiagnosticHistoryEntry["kind"], Check> = {
  signaling: shape({ ...common, kind: oneOf("signaling"), side: oneOf("Host", "Guest"),
    event: oneOf("created", "open", "disconnected", "close", "timeout", "error", "constructor-error"),
    error: optional(oneOf("browser-incompatible", "disconnected", "invalid-id", "invalid-key", "network", "peer-unavailable", "ssl-unavailable", "server-error", "socket-error", "socket-closed", "unavailable-id", "webrtc", "unknown")) }),
  credentials: shape({ ...common, kind: oneOf("credentials"), outcome: oneOf("fresh", "cache", "stun-fallback"),
    failure: optional(oneOf("http", "network", "invalid", "no-turn", "aborted")), httpStatus: optional(number) }),
  "connection-attempt": shape({ ...common, kind: oneOf("connection-attempt"), direction: oneOf("incoming", "outgoing"),
    event: oneOf("started", "open", "close", "error", "timeout", "aborted"),
    error: optional(oneOf("negotiation-failed", "connection-closed", "message-too-big", "unknown")), state: optional(shape(failureState)) }),
  "ice-candidate-error": shape({ ...common, kind: oneOf("ice-candidate-error"), code: number }),
  "candidate-route": shape({ ...common, kind: oneOf("candidate-route"), candidates }),
  disconnect: shape({ ...common, kind: oneOf("disconnect"), cause: oneOf("send-error", "connection-close", "connection-error", "remote-disconnect", "local-close"),
    error: optional(oneOf("negotiation-failed", "connection-closed", "message-too-big", "unknown")),
    transport, preClose: nullable(transport), candidates: optional(nullable(candidates)) }),
  "engine-not-initialized": shape({ ...common, kind: oneOf("engine-not-initialized"), engine,
    operation: oneOf("submitAction", "submitInteraction", "previewManaPayment", "previewInteraction", "getState", "getFilteredState", "getLegalActions", "getLegalActionsForViewer", "getSnapshot", "getViewerSnapshot", "getAiActionProposal", "getAiTacticalActionProposal", "submitAiActionProposal", "restoreState", "resumeRestoredGameState", "exportPersistenceState", "setMultiplayerMode", "applySeatMutation", "projectSeatView", "resumeMultiplayerHostState", "estimateBracket", "ping", "initializeGame", "initializeMultiplayerHostGame") }),
};
export function isRetainedDiagnosticEntry(value: unknown): value is DiagnosticHistoryEntry {
  if (!value || typeof value !== "object" || !("kind" in value) || !("observedAt" in value)) return false;
  if (typeof value.observedAt !== "number" || value.observedAt < Date.now() - DIAGNOSTIC_HISTORY_MAX_AGE_MS || value.observedAt > Date.now()) return false;
  return typeof value.kind === "string" && Object.prototype.hasOwnProperty.call(schemas, value.kind)
    && Object.entries(schemas).some(([kind, check]) => kind === value.kind && check(value));
}

/** Reject unknown fields and arbitrary strings at the storage boundary. */
export function loadDiagnosticHistory(): DiagnosticHistoryEntry[] {
  try {
    const raw = sessionStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    if (raw.length > MAX_BYTES) { sessionStorage.removeItem(STORAGE_KEY); return []; }
    const value: unknown = JSON.parse(raw);
    const entries = Array.isArray(value) ? value.slice(-DIAGNOSTIC_HISTORY_LIMIT).filter(isRetainedDiagnosticEntry) : [];
    saveDiagnosticHistory(entries);
    return entries;
  } catch { return []; }
}
export function saveDiagnosticHistory(entries: DiagnosticHistoryEntry[]): void {
  try {
    const retained = entries.slice(-DIAGNOSTIC_HISTORY_LIMIT).filter(isRetainedDiagnosticEntry);
    if (!retained.length) { sessionStorage.removeItem(STORAGE_KEY); return; }
    const raw = JSON.stringify(retained);
    if (raw.length <= MAX_BYTES) sessionStorage.setItem(STORAGE_KEY, raw);
  } catch { /* Storage restrictions must not interfere with gameplay or diagnostics. */ }
}
