import type { DataConnection } from "peerjs";

import type { P2PMessage } from "./protocol";
import { decodeWireMessage, encodeWireMessage } from "./protocol";

function tracePeerSession(event: string, data?: Record<string, unknown>): void {
  console.debug("[PeerSession Trace]", performance.now().toFixed(1), event, data ?? {});
}

export interface PeerSession {
  /**
   * Queue a message for the wire. Resolves `true` after the encoded bytes have
   * been handed to the underlying RTCDataChannel, or `false` if the queue entry
   * cannot be written because the channel closed, encoding failed, or the write
   * threw. The encode is async (CompressionStream), so production callers
   * awaiting this promise get a real "bytes are out" outcome — useful for
   * reconnect handshakes that must not promote a dead channel. Callers that
   * don't care about timing can ignore the promise.
   */
  send(msg: P2PMessage): Promise<boolean>;
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

  // Ping/pong keep-alive. We probe every `PING_INTERVAL_MS`; a channel that
  // has produced no pong for `PONG_TIMEOUT_MS` is declared dead. This is the
  // only detector for a HALF-OPEN channel — one where `conn.open` is still
  // true and `conn.on("close")` will never fire because nothing ever formally
  // closed.
  const PING_INTERVAL_MS = 5_000;
  const PONG_TIMEOUT_MS = 10_000;

  let pingInterval: ReturnType<typeof setInterval> | null = null;
  // `Date.now()` of the most recent pong, and of the previous interval tick.
  // Wall clock specifically because it is the clock a test can move
  // independently of the timer queue (`vi.setSystemTime`), which is what makes
  // the suspend discriminator below observable at all. Whether
  // `performance.now()` also advances across a suspend is browser-dependent and
  // is deliberately NOT the reason stated here.
  let lastPongAt = 0;
  let lastTickAt = 0;

  const clearKeepAlive = () => {
    if (pingInterval !== null) { clearInterval(pingInterval); pingInterval = null; }
  };

  // FIFO send queue. Compression is async (CompressionStream), so two rapid
  // trySend calls could race without ordering. The chain guarantees wire bytes
  // hit the DataChannel in submission order. Applied identically on receive.
  let sendQueue: Promise<void> = Promise.resolve();

  // Returns the promise representing this entry's slot in the queue. `true`
  // means `conn.send` accepted the encoded bytes; `false` means this entry
  // could not reach the channel. Channel-level send failures still trigger
  // `handleDisconnect` from inside the queue.
  const trySend = (msg: P2PMessage): Promise<boolean> => {
    if (closed || !conn.open) return Promise.resolve(false);
    const entry = sendQueue.then(async () => {
      // Only gate on `conn.open` here, NOT `closed`. `close()` flips `closed`
      // to true synchronously so subsequent NEW `trySend` calls bail (the
      // outer guard above), but already-queued entries — including the
      // `disconnect` farewell `close()` itself enqueues — still need to
      // flush before the channel is disposed.
      if (!conn.open) return false;
      let bytes: Uint8Array;
      try {
        bytes = await encodeWireMessage(msg);
      } catch (err) {
        // Encode failure is a programmer bug, not a channel failure. Log loud
        // but keep the channel alive for other (working) messages.
        console.error("[PeerSession] encode failed:", err, msg);
        return false;
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
        return true;
      } catch (err) {
        console.warn("[PeerSession] send failed:", err);
        handleDisconnect("Channel send failed");
        return false;
      }
    });
    sendQueue = entry.then(() => undefined);
    return entry;
  };

  const startKeepAlive = () => {
    lastPongAt = lastTickAt = Date.now();
    pingInterval = setInterval(() => {
      if (!conn.open) return;

      const now = Date.now();
      const sinceLastTick = now - lastTickAt;
      lastTickAt = now;

      // Suspend discriminator. A bare `now - lastPongAt` is NOT enough: a tab
      // frozen for five minutes and a channel silent for five minutes produce
      // the identical gap, and disconnecting every resumed tab (routine on
      // mobile) is worse than missing a dead channel. The gap since the
      // PREVIOUS TICK is what tells them apart — a tick that arrives a whole
      // silence budget after its predecessor did not observe the interval it
      // was scheduled for, so the elapsed time is no evidence about the peer.
      // Re-baseline and start measuring again.
      //
      // ACCEPTED CONSEQUENCE: re-baselining disables this detector for as long
      // as ticks keep arriving a full budget apart, and it does NOT distinguish
      // WHY they do. Any cause counts — a frozen tab, Chrome's intensive
      // throttling tier (hidden >= 5 min, ~1 tick/minute), or a long
      // synchronous WASM engine call blocking a FOREGROUNDED main thread. Note
      // ordinary hidden-tab throttling (~1/sec) does not delay a 5s interval at
      // all, so a merely-backgrounded tab usually keeps detecting normally.
      // Detection resumes two ticks after normal 5s pacing returns.
      //
      // The symmetric case is the PEER's, and this change newly reaches it: a
      // foregrounded tab sees healthy 5s gaps, so it never re-baselines, and it
      // will drop a peer whose page the browser has frozen for a whole budget.
      // Two things bound the damage. Pong replies ride `conn.on("data")`
      // rather than a timer, so ordinary hidden-tab throttling never silences a
      // peer; and a dropped peer auto-reconnects inside the host's 30s
      // `DEFAULT_GRACE_PERIOD_MS` (p2p-adapter.ts). A device locked past that
      // grace now loses the seat where it previously survived until ICE failed.
      //
      // That is the intended trade. The alternative — concluding silence from a
      // gap the tick cannot explain — false-disconnects a healthy peer, which
      // is the harm this whole branch exists to prevent.
      // `Date.now()` is wall-clock, so it can also move BACKWARD (an NTP step,
      // a manual clock change, a VM restore). That strands `lastPongAt` in the
      // future, and every later comparison then reads as "answered recently"
      // until the clock catches up — the detector silently disables itself for
      // the width of the jump. A negative gap is a discontinuity for the same
      // reason an oversized one is: the tick observed no interval it can vouch
      // for, so the elapsed time is no evidence about the peer. Re-baseline.
      if (sinceLastTick >= PONG_TIMEOUT_MS || sinceLastTick < 0) {
        lastPongAt = now;
      } else if (now - lastPongAt >= PONG_TIMEOUT_MS) {
        handleDisconnect("Ping timeout");
        return;
      }

      // Fire-and-forget: real `conn.send` failures fire `handleDisconnect`
      // from inside the queue's catch; the silence check above bounds
      // detection latency for everything else.
      void trySend({ type: "ping", timestamp: now });
    }, PING_INTERVAL_MS);
  };

  const beforeUnloadHandler = () => {
    // Best-effort farewell over the queued path. Compression is async, so the
    // message may not flush before the tab is torn down. If it doesn't, the
    // remote side falls back to its own keep-alive silence check (~10s), which
    // covers a foregrounded peer but is deliberately inert while that peer's
    // tab is backgrounded — see `startKeepAlive`.
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
        // Sole liveness evidence the keep-alive tick reads.
        lastPongAt = Date.now();
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
        // Flush buffered messages through the same serialized recvQueue used by
        // onData, rather than dispatching them synchronously and un-awaited.
        // That keeps three guarantees the engine relies on:
        //  - async handlers are awaited, so a handler-triggered send completes
        //    before the next inbound message is dispatched (ordering invariant);
        //  - the buffered messages stay ordered relative to any inbound message
        //    already queued on recvQueue;
        //  - a throwing/rejecting handler is caught here instead of dropping an
        //    unhandled rejection or breaking the chain (matches onData).
        recvQueue = recvQueue
          .catch(() => {})
          .then(async () => {
            for (const msg of queued) {
              try {
                await handler(msg);
              } catch (e) {
                console.warn("[PeerSession] pending message handler threw:", e, msg.type);
              }
            }
          });
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
