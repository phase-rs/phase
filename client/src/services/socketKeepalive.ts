import type { PhaseSocketTransport } from "./openPhaseSocket";

/**
 * Period of the application ping, matching `WebSocketAdapter.startPing`. The
 * public edge closes an idle WebSocket at ~125 s, so this leaves ~25x headroom
 * — enough that a throttled background tab still holds the socket.
 */
export const KEEPALIVE_INTERVAL_MS = 5000;

/**
 * Starts the application ping on a socket whose owner keeps it open across
 * idle periods, and returns the stopper. Deliberately not part of
 * `openPhaseSocket`: consumers legitimately replace the handshake wholesale,
 * and a keepalive belongs to a socket's owner rather than to how it was opened.
 *
 * The stopper is idempotent, so a per-socket close handler and an owner-level
 * teardown may both call it. The ping also stops itself once the socket is
 * past OPEN: a connection can reach CLOSED without ever emitting `close`, and
 * an owner that installed only a close listener would otherwise leak the
 * interval for the life of the page.
 */
export function startSocketKeepalive(
  ws: Pick<PhaseSocketTransport, "send" | "readyState">,
): () => void {
  const timer = setInterval(() => {
    if (ws.readyState === WebSocket.CLOSING || ws.readyState === WebSocket.CLOSED) {
      clearInterval(timer);
      return;
    }
    if (ws.readyState !== WebSocket.OPEN) return;
    ws.send(JSON.stringify({ type: "Ping", data: { timestamp: Date.now() } }));
  }, KEEPALIVE_INTERVAL_MS);
  return () => clearInterval(timer);
}
