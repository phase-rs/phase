import type { DataConnection } from "peerjs";

import { trackEvent } from "../services/telemetry";
import { boundedDiagnosticProbe, diagnosticIdFor, projectCandidateStats, recordDiagnostic, registerPeerDiagnostics } from "../services/troubleshooting";
import type { CandidateDiagnosticSnapshot, ConnectionDiagnosticError, DisconnectCause, TransportDiagnosticSnapshot } from "../services/troubleshooting";
import { safeConnectionError } from "./connection";
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
  /** Round-trip latency, or null when the last measurement is stale. */
  onLatency?: (latencyMs: number | null) => void;
}

export function createPeerSession(
  conn: DataConnection,
  options: PeerSessionOptions = {},
): PeerSession {
  tracePeerSession("create-session", { connOpen: conn.open });
  const { onSessionEnd } = options;
  const diagnosticId = diagnosticIdFor(conn);
  const messageHandlers = new Set<(msg: P2PMessage) => void | Promise<void>>();
  const disconnectHandlers = new Set<(reason: string) => void>();
  let closed = false;
  let disconnectReason: string | null = null;

  const pendingMessages: P2PMessage[] = [];

  // Probes measure latency only. A delayed pong is not evidence that a live
  // WebRTC channel should be destroyed (state traffic can still be arriving).
  const PING_INTERVAL_MS = 5_000;
  const LATENCY_STALE_MS = 15_000;
  let pingInterval: ReturnType<typeof setInterval> | null = null;
  let lastPongAt = Date.now();
  let latencyStale = false;
  let lastReceivedAt: number | null = null;
  let lastReceivedType: P2PMessage["type"] | "" = "";
  let pendingSends = 0;
  let pendingDecodes = 0;

  // PeerJS clears these fields during close; native objects alone are also
  // insufficient because their state mutates to "closed" before its callback.
  const peerConnection = conn.peerConnection;
  const dataChannel = conn.dataChannel;
  let hasPong = false;
  let connectionError: ConnectionDiagnosticError | undefined;
  let channelError: TransportDiagnosticSnapshot["channelError"] = null;
  let preClose: TransportDiagnosticSnapshot | null = null;
  const sampleTransport = (): TransportDiagnosticSnapshot => {
    const now = Date.now();
    const snapshot: TransportDiagnosticSnapshot = {
      observedAt: now,
      connectionState: peerConnection?.connectionState ?? null,
      iceState: peerConnection?.iceConnectionState ?? null,
      channelState: dataChannel?.readyState ?? null,
      bufferedBytes: dataChannel?.bufferedAmount ?? null,
      pendingSends, pendingDecodes,
      receiveAgeMs: lastReceivedAt === null ? null : Math.max(0, now - lastReceivedAt),
      pongAgeMs: hasPong ? Math.max(0, now - lastPongAt) : null,
      channelError,
    };
    if (!closed && snapshot.connectionState !== "closed"
      && snapshot.iceState !== "closed" && snapshot.channelState !== "closed"
      && snapshot.channelState !== "closing") preClose = snapshot;
    return snapshot;
  };
  let retainedCandidates: CandidateDiagnosticSnapshot | null = null;
  let statsPending: Promise<CandidateDiagnosticSnapshot | null> | null = null;
  let statsRerun = false;
  const transportClosed = () => closed || peerConnection?.connectionState === "closed"
    || peerConnection?.iceConnectionState === "closed" || dataChannel?.readyState === "closing"
    || dataChannel?.readyState === "closed";
  const sampleCandidates = async (): Promise<CandidateDiagnosticSnapshot | null> => {
    if (transportClosed() || !peerConnection?.getStats) return retainedCandidates;
    if (statsPending) { statsRerun = true; return statsPending; }
    statsPending = (async () => {
      do {
        statsRerun = false;
        try {
          const candidates = await boundedDiagnosticProbe(async () => {
            // The bounded probe begins on a microtask; teardown can run before it.
            if (transportClosed()) return null;
            return projectCandidateStats(await peerConnection.getStats());
          });
          if (!transportClosed() && candidates) {
            retainedCandidates = candidates;
            recordDiagnostic({ kind: "candidate-route", diagnosticId, observedAt: candidates.observedAt ?? Date.now(), candidates });
          }
        } catch { /* Unsupported or already closed transports have no new route evidence. */ }
      } while (statsRerun && !transportClosed());
      return retainedCandidates;
    })().finally(() => { statsPending = null; });
    return statsPending;
  };
  const onTransportState = () => { sampleTransport(); void sampleCandidates(); };
  const onChannelError = () => { channelError = "data-channel-error"; sampleTransport(); };
  peerConnection?.addEventListener?.("connectionstatechange", onTransportState);
  peerConnection?.addEventListener?.("iceconnectionstatechange", onTransportState);
  for (const event of ["open", "closing", "close"]) dataChannel?.addEventListener?.(event, onTransportState);
  dataChannel?.addEventListener?.("error", onChannelError);
  sampleTransport();
  void sampleCandidates();
  const unregisterDiagnostics = registerPeerDiagnostics({
    diagnosticId,
    snapshot: sampleTransport,
    stats: sampleCandidates,
  });

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
    pendingSends += 1;
    sampleTransport();
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
        sampleTransport();
        return true;
      } catch (err) {
        console.warn("[PeerSession] send failed:", err);
        handleDisconnect("Channel send failed", "send-error");
        return false;
      }
    });
    sendQueue = entry.then(() => { pendingSends -= 1; sampleTransport(); });
    return entry;
  };

  const startKeepAlive = () => {
    pingInterval = setInterval(() => {
      sampleTransport();
      if (!conn.open) return;
      const now = Date.now();
      if (!latencyStale && (now - lastPongAt >= LATENCY_STALE_MS || now < lastPongAt)) {
        latencyStale = true;
        options.onLatency?.(null);
      }
      void trySend({ type: "ping", timestamp: now });
    }, PING_INTERVAL_MS);
  };

  const beforeUnloadHandler = () => {
    // Best-effort farewell over the queued path. Compression is async, so the
    // message may not flush before the tab is torn down. If it doesn't, the
    // remote side relies on WebRTC channel closure/error detection.
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
  const markDisconnected = (reason: string, cause: DisconnectCause) => {
    if (closed) return;
    const transport = sampleTransport();
    closed = true;
    recordDiagnostic({ kind: "disconnect", diagnosticId, observedAt: Date.now(), cause, ...(connectionError ? { error: connectionError } : {}), transport, preClose, candidates: retainedCandidates });
    unregisterDiagnostics();
    peerConnection?.removeEventListener?.("connectionstatechange", onTransportState);
    peerConnection?.removeEventListener?.("iceconnectionstatechange", onTransportState);
    for (const event of ["open", "closing", "close"]) dataChannel?.removeEventListener?.(event, onTransportState);
    dataChannel?.removeEventListener?.("error", onChannelError);
    disconnectReason = reason;
    tracePeerSession("disconnect", { reason, connOpen: conn.open });
    console.warn("[PeerSession] disconnected:", reason);
    // Only bounded transport metadata: never upload peer IDs, room codes,
    // message payloads, or the free-form reason supplied by a remote peer.
    const now = Date.now();
    trackEvent("p2p_disconnect", {
      reason: cause,
      connection_state: peerConnection?.connectionState ?? "",
      ice_state: peerConnection?.iceConnectionState ?? "",
      visibility: document.visibilityState,
      last_message_type: lastReceivedType,
      pong_age_ms: Math.max(0, now - lastPongAt),
      receive_age_ms: lastReceivedAt === null ? -1 : Math.max(0, now - lastReceivedAt),
      pending_sends: pendingSends,
      pending_decodes: pendingDecodes,
      buffered_bytes: dataChannel?.bufferedAmount ?? 0,
      channel_open: conn.open,
      last_connection_state: preClose?.connectionState ?? "",
      last_ice_state: preClose?.iceState ?? "",
      last_channel_state: preClose?.channelState ?? "",
      channel_error: channelError ?? "",
      transport_captured_at: preClose?.observedAt ?? 0,
      last_buffered_bytes: preClose?.bufferedBytes ?? -1,
      transport_age_ms: preClose === null ? -1 : Math.max(0, now - preClose.observedAt),
    });
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
  const handleDisconnect = (reason: string, cause: DisconnectCause) => {
    if (closed) return;
    markDisconnected(reason, cause);
    disposeChannel();
  };

  // Decode in wire order, but dispatch game messages on a separate FIFO.
  // An async engine action must not strand an already-arrived ping/pong behind
  // its handler: the keep-alive would otherwise close a healthy channel.
  let recvQueue: Promise<void> = Promise.resolve();
  let dispatchQueue: Promise<void> = Promise.resolve();

  // Returns this message's delivery promise. Production callers (PeerJS event
  // emitter) ignore it; the test fake uses it to deterministically await the
  // full inbound chain.
  const onData = (data: unknown): Promise<void> => {
    let delivery: Promise<void> | undefined;
    pendingDecodes += 1;
    recvQueue = recvQueue.then(async () => {
      if (closed) return;
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
      lastReceivedAt = Date.now();
      lastReceivedType = msg.type;
      sampleTransport();
      // Skip ping/pong — they fire every 5s and drown the rest of the trace.
      if (msg.type !== "ping" && msg.type !== "pong") {
        tracePeerSession("data", { type: msg.type, queued: messageHandlers.size === 0 });
      }

      if (msg.type === "pong") {
        const elapsed = Date.now() - msg.timestamp;
        if (!Number.isFinite(elapsed) || elapsed < 0) return;
        lastPongAt = Date.now();
        hasPong = true;
        sampleTransport();
        latencyStale = false;
        options.onLatency?.(Math.round(elapsed));
        return;
      }

      if (msg.type === "ping") {
        void trySend({ type: "pong", timestamp: msg.timestamp });
        return;
      }

      delivery = dispatchQueue.then(async () => {
        if (closed) return;
        if (msg.type === "disconnect") {
          handleDisconnect(msg.reason, "remote-disconnect");
          return;
        }

        if (messageHandlers.size === 0) {
          pendingMessages.push(msg);
          return;
        }

        // Await each game handler to preserve action/state ordering. Catch
        // failures per handler so a rejection cannot poison later deliveries.
        for (const message of [...pendingMessages.splice(0), msg]) {
          for (const handler of messageHandlers) {
            try {
              await handler(message);
            } catch (e) {
              console.warn("[PeerSession] message handler threw:", e, message.type);
            }
          }
        }
      });
      dispatchQueue = delivery;
    }).finally(() => { pendingDecodes -= 1; });
    // Tests can await this message's full delivery without making the decode
    // queue itself wait for game work.
    return recvQueue.then(() => delivery);
  };

  conn.on("data", onData);
  conn.on("close", () => handleDisconnect("Connection closed", "connection-close"));
  conn.on("error", (err) => {
    connectionError = safeConnectionError(err);
    handleDisconnect(`Connection error: ${err.message}`, "connection-error");
  });

  startKeepAlive();

  return {
    send(msg) {
      return trySend(msg);
    },
    onMessage(handler) {
      messageHandlers.add(handler);

      if (pendingMessages.length > 0) {
        // Flush buffered messages through the same serialized dispatchQueue used by
        // onData, rather than dispatching them synchronously and un-awaited.
        // That keeps three guarantees the engine relies on:
        //  - async handlers are awaited, so a handler-triggered send completes
        //    before the next inbound message is dispatched (ordering invariant);
        //  - the buffered messages stay ordered relative to any inbound message
        //    already queued on dispatchQueue;
        //  - a throwing/rejecting handler is caught here instead of dropping an
        //    unhandled rejection or breaking the chain (matches onData).
        dispatchQueue = dispatchQueue
          .catch(() => {})
          .then(async () => {
            // Drain at execution time: an earlier queued delivery may already
            // have flushed these messages after this listener subscribed.
            for (const msg of pendingMessages.splice(0)) {
              if (closed) return;
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
      markDisconnected(reason, "local-close");
      sendQueue = sendQueue.then(() => { disposeChannel(); });
    },
  };
}
