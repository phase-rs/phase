import type { DataConnection } from "peerjs";

import type { P2PMessage } from "./protocol";
import { decodeWireMessage, encodeWireMessage } from "./protocol";

function tracePeerSession(event: string, data?: Record<string, unknown>): void {
  console.debug("[PeerSession Trace]", performance.now().toFixed(1), event, data ?? {});
}

export interface PeerSession {
  /**
   * Queue a message for the wire. Resolves after the encoded bytes have been
   * written to the underlying RTCDataChannel (or after the queue entry is
   * dropped due to channel closure). The encode is async (CompressionStream),
   * so production callers awaiting this promise get a real "bytes are out"
   * guarantee — useful for fan-out broadcast sites that need to settle all
   * sends before returning, and for deterministic test assertions. Callers
   * that don't care about timing can ignore the promise.
   */
  send(msg: P2PMessage): Promise<void>;
  onMessage(handler: (msg: P2PMessage) => void | Promise<void>): () => void;
  onDisconnect(handler: (reason: string) => void): () => void;
  close(reason?: string): void;
}

export interface PeerSessionOptions {
  /**
   * Optional callback invoked exactly once when this session ends, after
   * `disconnectHandlers` have run. Use this to release per-session resources
   * (e.g., remove the session from a map of active guests). DO NOT destroy the
   * parent `Peer` here — that would cascade-kill all sibling sessions in a
   * hub-and-spoke (multi-guest) host setup. Peer lifetime is owned by the
   * adapter that created the `Peer`.
   */
  onSessionEnd?: () => void;
}

export function createPeerSession(
  conn: DataConnection,
  options: PeerSessionOptions = {},
): PeerSession {
  tracePeerSession("create-session", { connOpen: conn.open });
  const { onSessionEnd } = options;
  const messageHandlers = new Set<(msg: P2PMessage) => void | Promise<void>>();
  const disconnectHandlers = new Set<(reason: string) => void>();
  let closed = false;
  let disconnectReason: string | null = null;

  const pendingMessages: P2PMessage[] = [];

  // Ping/pong keep-alive
  let pingInterval: ReturnType<typeof setInterval> | null = null;
  let pongTimeout: ReturnType<typeof setTimeout> | null = null;

  const clearKeepAlive = () => {
    if (pingInterval !== null) { clearInterval(pingInterval); pingInterval = null; }
    if (pongTimeout !== null) { clearTimeout(pongTimeout); pongTimeout = null; }
  };

  // FIFO send queue. Compression is async (CompressionStream), so two rapid
  // trySend calls could race without ordering. The chain guarantees wire bytes
  // hit the DataChannel in submission order. Applied identically on receive.
  let sendQueue: Promise<void> = Promise.resolve();

  // Returns the promise representing this entry's slot in the queue — resolves
  // after the encoded bytes hit `conn.send` (or after the entry is dropped
  // because the channel closed). Production callers that don't care about
  // timing simply ignore the promise. Channel-level send failures still
  // trigger `handleDisconnect` from inside the queue.
  const trySend = (msg: P2PMessage): Promise<void> => {
    if (closed || !conn.open) return Promise.resolve();
    const entry = sendQueue.then(async () => {
      // Only gate on `conn.open` here, NOT `closed`. `close()` flips `closed`
      // to true synchronously so subsequent NEW `trySend` calls bail (the
      // outer guard above), but already-queued entries — including the
      // `disconnect` farewell `close()` itself enqueues — still need to
      // flush before the channel is disposed.
      if (!conn.open) return;
      let bytes: Uint8Array;
      try {
        bytes = await encodeWireMessage(msg);
      } catch (err) {
        // Encode failure is a programmer bug, not a channel failure. Log loud
        // but keep the channel alive for other (working) messages.
        console.error("[PeerSession] encode failed:", err, msg);
        return;
      }
      if (msg.type !== "ping" && msg.type !== "pong") {
        const rawSize = JSON.stringify(msg).length;
        const reduction = rawSize > 0 ? ((1 - bytes.length / rawSize) * 100).toFixed(0) : "0";
        console.log(
          `[PeerSession] sending "${msg.type}" (${(bytes.length / 1024).toFixed(1)} KB wire, ${(rawSize / 1024).toFixed(1)} KB raw, ${reduction}% reduction)`,
        );
        tracePeerSession("send", { type: msg.type, connOpen: conn.open, size: bytes.length });
      }
      try {
        conn.send(bytes);
      } catch (err) {
        console.warn("[PeerSession] send failed:", err);
        handleDisconnect("Channel send failed");
      }
    });
    sendQueue = entry;
    return entry;
  };

  const startKeepAlive = () => {
    pingInterval = setInterval(() => {
      if (!conn.open) return;

      if (pongTimeout !== null) { clearTimeout(pongTimeout); pongTimeout = null; }

      // Fire-and-forget: real `conn.send` failures fire `handleDisconnect`
      // from inside the queue's catch; the 10s pong-timeout below bounds
      // detection latency for everything else.
      void trySend({ type: "ping", timestamp: Date.now() });

      pongTimeout = setTimeout(() => {
        if (!closed) handleDisconnect("Ping timeout");
      }, 10_000);
    }, 5_000);
  };

  const beforeUnloadHandler = () => {
    // Best-effort farewell over the queued path. Compression is async, so the
    // message may not flush before the tab is torn down; the 10s pong-timeout
    // is the reliable disconnect detection on the remote side regardless.
    if (!closed && conn.open) void trySend({ type: "disconnect", reason: "Page closed" });
  };
  window.addEventListener("beforeunload", beforeUnloadHandler);

  // Two-phase disconnect:
  //   markDisconnected — sync: sets `closed`, fires disconnectHandlers and
  //     `onSessionEnd`. Subsequent `trySend` calls bail. Subsequent
  //     `onDisconnect` subscribers fire immediately. Does NOT close the
  //     RTCDataChannel — already-queued sends still need it to be open.
  //   disposeChannel — closes the RTCDataChannel. Called either directly
  //     (from `conn.on("close"/"error")` paths where there are no queued
  //     sends to flush) or chained off `sendQueue` (from `close()`).
  const markDisconnected = (reason: string) => {
    if (closed) return;
    closed = true;
    disconnectReason = reason;
    tracePeerSession("disconnect", { reason, connOpen: conn.open });
    console.warn("[PeerSession] disconnected:", reason);
    clearKeepAlive();
    window.removeEventListener("beforeunload", beforeUnloadHandler);
    for (const handler of disconnectHandlers) {
      handler(reason);
    }
    if (onSessionEnd) {
      try { onSessionEnd(); } catch (e) {
        console.warn("onSessionEnd handler threw:", e);
      }
    }
  };

  const disposeChannel = () => {
    // Best-effort. Do NOT touch the parent `Peer` — that lifetime is owned
    // by the creator of the `Peer` (host adapter / guest adapter).
    try { conn.close(); } catch (e) {
      console.warn("Error closing data connection:", e);
    }
  };

  // Backwards-compatible bundled handler used by remote-close / error paths
  // where there is no queued-send-flush to await.
  const handleDisconnect = (reason: string) => {
    if (closed) return;
    markDisconnected(reason);
    disposeChannel();
  };

  // FIFO receive queue mirrors the send queue. DecompressionStream is async,
  // so concurrent onData invocations must be serialized to preserve the
  // state_update N → state_update N+1 ordering invariant the engine depends on.
  let recvQueue: Promise<void> = Promise.resolve();

  // Returns the recvQueue entry's promise. Production callers (PeerJS event
  // emitter) ignore it; the test fake uses it to deterministically await the
  // full inbound chain.
  const onData = (data: unknown): Promise<void> => {
    recvQueue = recvQueue.then(async () => {
      if (!(data instanceof Uint8Array || data instanceof ArrayBuffer)) {
        // PeerJS "binary" mode can deliver either Uint8Array or ArrayBuffer
        // depending on msgpack unwrap path. Anything else means a version
        // mismatch (old-bundle peer sending plain JSON objects) or corruption.
        console.warn("[PeerSession] received non-binary message; dropping:", typeof data);
        return;
      }
      const bytes = data instanceof ArrayBuffer ? new Uint8Array(data) : data;
      let msg: P2PMessage;
      try {
        msg = await decodeWireMessage(bytes);
      } catch (e) {
        console.warn("Failed to decode message from peer:", e);
        return;
      }
      // Skip ping/pong — they fire every 5s and drown the rest of the trace.
      if (msg.type !== "ping" && msg.type !== "pong") {
        tracePeerSession("data", { type: msg.type, queued: messageHandlers.size === 0 });
      }

      if (msg.type === "pong") {
        if (pongTimeout !== null) { clearTimeout(pongTimeout); pongTimeout = null; }
        return;
      }

      if (msg.type === "ping") {
        void trySend({ type: "pong", timestamp: msg.timestamp });
        return;
      }

      if (msg.type === "disconnect") {
        handleDisconnect(msg.reason);
        return;
      }

      if (messageHandlers.size === 0) {
        pendingMessages.push(msg);
        return;
      }

      // Await async handlers so the recvQueue chain reflects the full
      // chain — handler-triggered sends complete before the next inbound
      // message is dispatched. Sync handlers return undefined; awaiting
      // it is a no-op microtask.
      //
      // Per-handler try/catch: a thrown handler must NOT reject the
      // recvQueue promise. `.then(onFulfilled)` without `onRejected`
      // propagates rejection forward, so the next onData would skip its
      // body and silently freeze inbound dispatch for the rest of the
      // session. Logging here is the same posture as decodeWireMessage's
      // catch above — keep the channel alive, surface the error.
      for (const handler of messageHandlers) {
        try {
          await handler(msg);
        } catch (e) {
          console.warn("[PeerSession] message handler threw:", e, msg.type);
        }
      }
    });
    return recvQueue;
  };

  conn.on("data", onData);
  conn.on("close", () => handleDisconnect("Connection closed"));
  conn.on("error", (err) => handleDisconnect(`Connection error: ${err.message}`));

  startKeepAlive();

  return {
    send(msg) {
      return trySend(msg);
    },
    onMessage(handler) {
      messageHandlers.add(handler);

      if (pendingMessages.length > 0) {
        const queued = pendingMessages.splice(0);
        for (const msg of queued) {
          handler(msg);
        }
      }

      return () => {
        messageHandlers.delete(handler);
      };
    },
    onDisconnect(handler) {
      disconnectHandlers.add(handler);

      if (disconnectReason !== null) {
        handler(disconnectReason);
      }

      return () => {
        disconnectHandlers.delete(handler);
      };
    },
    close(reason = "Left game") {
      if (closed) return;
      // Order matters: queue the farewell + any caller-pending sends, mark
      // disconnected synchronously (so `onDisconnect`-after-`close` fires
      // immediately as the API contract requires), THEN dispose the channel
      // after the queue drains so the queued bytes actually flush.
      if (conn.open) trySend({ type: "disconnect", reason });
      markDisconnected(reason);
      sendQueue = sendQueue.then(() => { disposeChannel(); });
    },
  };
}
