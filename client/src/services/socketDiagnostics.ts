import type { PhaseSocketTransport } from "./openPhaseSocket";
import { WEBSOCKET_CLOSE_REASONS } from "./diagnosticHistory";
import { diagnosticIdFor, recordDiagnostic, type SocketReconnectOutcome } from "./troubleshooting";

export interface SocketDiagnosticObserver {
  didSend(): void;
  markSilenceExceeded(): void;
  detach(): void;
}

/** Observe raw frames before gzip decoding; never retain payloads, endpoints or credentials. */
export function observeSocketDiagnostics(
  socket: PhaseSocketTransport,
  reconnectAttempt: number,
  onLoss: (diagnosticId: string) => SocketReconnectOutcome,
  onInbound: () => void,
): SocketDiagnosticObserver {
  const diagnosticId = diagnosticIdFor(socket);
  let lastReceivedAt: number | null = null;
  let lastSentAt: number | null = null;
  let silenceExceeded = false;
  let lost = false;
  const receive = () => { lastReceivedAt = Date.now(); onInbound(); };
  const detach = () => {
    socket.removeEventListener("message", receive);
    socket.removeEventListener("close", close);
  };
  const close = (event: CloseEvent) => {
    if (lost) return;
    lost = true;
    detach();
    const now = Date.now();
    const rawReason = typeof event.reason === "string" ? event.reason : "";
    // Truncation alone cannot scrub a token. Retain only known public phrases.
    const reason = WEBSOCKET_CLOSE_REASONS.find((value) => value === rawReason.toLowerCase()) ?? "redacted";
    recordDiagnostic({
      kind: "websocket-loss", diagnosticId, observedAt: now,
      code: typeof event.code === "number" ? event.code : null,
      reason, reasonLength: rawReason.length, wasClean: event.wasClean === true,
      receiveAgeMs: lastReceivedAt === null ? null : Math.max(0, now - lastReceivedAt),
      sendAgeMs: lastSentAt === null ? null : Math.max(0, now - lastSentAt),
      visibility: document.visibilityState, online: navigator.onLine,
      silenceExceeded, reconnectAttempt, outcome: onLoss(diagnosticId),
    });
  };
  socket.addEventListener("message", receive);
  socket.addEventListener("close", close);
  return {
    didSend: () => { lastSentAt = Date.now(); },
    markSilenceExceeded: () => { silenceExceeded = true; },
    detach,
  };
}
