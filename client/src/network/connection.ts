import Peer from "peerjs";
import type { DataConnection } from "peerjs";

/** Unambiguous characters -- no 0/O, 1/I/L confusion */
const CODE_ALPHABET = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_LENGTH = 5;
/**
 * Namespace prefix for PeerJS IDs on the shared `0.peerjs.com` signaling
 * server. Without it, bare 5-char codes collide with rooms hosted by any
 * other PeerJS-based app on the internet (and `new Peer(peerId)` would fail
 * with `unavailable-id`). Keep in sync with `stripPeerIdPrefix` consumers.
 */
// Bumped from "phase-" → "phase2-" when the binary wire format shipped:
// old-bundle clients (JSON serialization) connecting to a new-bundle host
// (binary serialization) would silently corrupt every message. The prefix
// bump causes old-bundle peers to fail with `unavailable-id` instead of
// connecting and garbling — a clean, actionable failure mode. Persisted
// reconnect tokens survive the bump because they key on bare roomCode, not
// peerId.
const PEER_ID_PREFIX = "phase2-";

/**
 * Strip the PeerJS namespace prefix so a peer id from any source (broker
 * response, legacy storage, current host) can be normalized back to the bare
 * 5-char room code for re-prefixing by `joinRoom`. Safe for values that
 * were never prefixed.
 */
export function stripPeerIdPrefix(peerId: string): string {
  return peerId.startsWith(PEER_ID_PREFIX)
    ? peerId.slice(PEER_ID_PREFIX.length)
    : peerId;
}

// Override PeerJS defaults -- their bundled TURN servers are broken
const PEER_CONFIG: RTCConfiguration = {
  iceServers: [
    { urls: "stun:stun.relay.metered.ca:80" },
    {
      urls: "turn:global.relay.metered.ca:80",
      username: "a267722d3eb02873687da73c",
      credential: "ob5D/eUnCmkkf1Vp",
    },
    {
      urls: "turn:global.relay.metered.ca:80?transport=tcp",
      username: "a267722d3eb02873687da73c",
      credential: "ob5D/eUnCmkkf1Vp",
    },
    {
      urls: "turn:global.relay.metered.ca:443",
      username: "a267722d3eb02873687da73c",
      credential: "ob5D/eUnCmkkf1Vp",
    },
    {
      urls: "turns:global.relay.metered.ca:443?transport=tcp",
      username: "a267722d3eb02873687da73c",
      credential: "ob5D/eUnCmkkf1Vp",
    },
  ],
};

function traceP2P(side: "Host" | "Guest", event: string, data?: Record<string, unknown>): void {
  console.debug(`[P2P ${side} Trace]`, performance.now().toFixed(1), event, data ?? {});
}

// ICE nomination typically settles within 1-2s of channel open; on slow links
// nomination may take longer. The first stat may be a `prflx` that later
// upgrades to `srflx`/`host`. 2000ms is a heuristic balance between accuracy
// and user-visible log latency.
const ICE_SETTLE_MS = 2000;

/**
 * Log the selected ICE candidate pair for a DataConnection, distinguishing
 * direct (host/srflx/prflx) from TURN-relayed sessions. Pure observability —
 * never throws upward. Called once per session after `conn.open`.
 *
 * Critical for TURN-bandwidth diagnostics: TURN-relayed sessions pay 2x the
 * application traffic (ingress + egress on the relay), and we have a free-tier
 * quota. Without this log we cannot tell whether bandwidth burn is due to
 * payload size or TURN-relay multiplication.
 */
// lib.dom.d.ts exposes `RTCIceCandidatePairStats` but not `RTCIceCandidateStats`
// in the TS version this project ships with; define the fields we read
// structurally to stay independent of lib.dom version drift.
interface IceCandidateStats {
  id: string;
  candidateType?: string;
  protocol?: string;
}

// Minimal DataConnection surface we need — tests can supply mocks without
// reconstructing the full RTCPeerConnection/DataConnection type hierarchy.
export interface IceStatsSource {
  peerConnection?: Pick<RTCPeerConnection, "getStats"> | undefined;
}

export async function logSelectedIceCandidate(
  side: "Host" | "Guest",
  conn: IceStatsSource,
): Promise<void> {
  try {
    await new Promise((r) => setTimeout(r, ICE_SETTLE_MS));
    const pc = conn.peerConnection;
    if (!pc) return;
    const stats = await pc.getStats();
    let pair: RTCIceCandidatePairStats | undefined;
    const candidates = new Map<string, IceCandidateStats>();
    stats.forEach((report) => {
      if (report.type === "candidate-pair") {
        const p = report as RTCIceCandidatePairStats;
        if (p.nominated && p.state === "succeeded") {
          pair = p;
        }
      } else if (report.type === "local-candidate" || report.type === "remote-candidate") {
        candidates.set(report.id, report as IceCandidateStats);
      }
    });
    if (!pair) return;
    const local = pair.localCandidateId ? candidates.get(pair.localCandidateId) : undefined;
    const remote = pair.remoteCandidateId ? candidates.get(pair.remoteCandidateId) : undefined;
    const localType = local?.candidateType;
    const remoteType = remote?.candidateType;
    const relayed = localType === "relay" || remoteType === "relay";
    const marker = relayed ? "⚠️ RELAYED VIA TURN (paid bandwidth)" : "✓ direct";
    console.log(
      `[ICE ${side}] selected pair: local=${localType}/${local?.protocol} remote=${remoteType}/${remote?.protocol} ${marker}`,
    );
    traceP2P(side, "ice-candidate-pair", { localType, remoteType, relayed });
  } catch (err) {
    console.warn(`[ICE ${side}] getStats failed:`, err);
  }
}

export interface HostResult {
  roomCode: string;
  peerId: string;
  /**
   * The signaling-server-registered `Peer`. Exposed so the host adapter can
   * subscribe to `peer.on("connection", ...)` directly when it needs the
   * guest's PlayerId in scope at wrap time. Most callers should prefer
   * `onGuestConnected` instead — the `Peer` reference is for advanced cases.
   */
  peer: Peer;
  /**
   * Subscribe to incoming guest connections. Multi-fire: handler is called for
   * every new guest after their `DataConnection.open` event. Returns an
   * unsubscribe function.
   *
   * Each `DataConnection` is delivered to the host adapter, which is
   * responsible for wrapping it in a `PeerSession` (with its own
   * `onSessionEnd` callback) and tracking the per-guest lifecycle.
   */
  onGuestConnected: (handler: (conn: DataConnection) => void) => () => void;
  /**
   * Tear down the shared `Peer`. Sole authoritative cleanup site for the
   * underlying signaling-server connection. Per-session disconnects must NOT
   * call this — that would cascade-kill all sibling guests.
   */
  destroy: () => void;
}

export interface JoinResult {
  conn: DataConnection;
  peer: Peer;
  /** Close only the current `DataConnection` (e.g., user-initiated leave of one room while rejoining another). */
  closeConn: () => void;
  /** Tear down the entire `Peer`. Sole authoritative cleanup. Auto-reconnect must NOT call this. */
  destroyPeer: () => void;
}

export function generateRoomCode(): string {
  const chars: string[] = [];
  for (let i = 0; i < CODE_LENGTH; i++) {
    chars.push(CODE_ALPHABET[Math.floor(Math.random() * CODE_ALPHABET.length)]);
  }
  return chars.join("");
}

/**
 * Validate and normalize a room code from user input.
 * Returns the uppercase code or null if invalid.
 */
export function parseRoomCode(input: string): string | null {
  const code = input.trim().toUpperCase();
  if (code.length !== CODE_LENGTH) return null;
  for (const ch of code) {
    if (!CODE_ALPHABET.includes(ch)) return null;
  }
  return code;
}

export interface HostRoomOptions {
  /**
   * Reuse a specific room code instead of generating a random one. Used
   * by host-resume flows to dial back in on the same peer id so guests'
   * persisted tokens (keyed on `phase-<roomCode>`) still match.
   *
   * If the PeerJS signaling server still holds the prior registration
   * (e.g., the old Peer hasn't fully GC'd), registration fails with
   * `unavailable-id`. `hostRoom` retries 3x with 3s backoff; if all
   * retries fail, it rejects — the caller decides whether to surface
   * "try again later" vs. fall back to a fresh code. We NEVER silently
   * swap to a fresh code: that would orphan every guest's persisted
   * token.
   */
  preferredRoomCode?: string;
}

const UNAVAILABLE_ID_RETRY_BACKOFF_MS = [3_000, 3_000, 3_000];

/**
 * Attempt to register a host Peer on the signaling server, retrying
 * on `unavailable-id` when `allowUnavailableIdRetry` is set. Each
 * attempt uses a fresh `Peer` instance — PeerJS objects are single-use
 * after an error, so retrying requires full reconstruction.
 *
 * Throws `AbortError` if the signal fires, a preserved `Error` with
 * an `.cause` carrying the PeerJS error type on failure, or resolves
 * with the opened Peer on success.
 */
async function openHostPeer(
  peerId: string,
  roomCode: string,
  allowUnavailableIdRetry: boolean,
  signal?: AbortSignal,
): Promise<Peer> {
  const maxAttempts = allowUnavailableIdRetry
    ? UNAVAILABLE_ID_RETRY_BACKOFF_MS.length + 1
    : 1;

  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    if (signal?.aborted) throw new DOMException("Aborted", "AbortError");

    const peer = new Peer(peerId, { config: PEER_CONFIG });
    traceP2P("Host", "create-peer", { roomCode, peerId, attempt });

    try {
      await new Promise<void>((resolve, reject) => {
        const onAbort = () => {
          traceP2P("Host", "abort-before-open", { peerId });
          peer.off("open", onOpen);
          peer.off("error", onError);
          try { peer.destroy(); } catch { /* best-effort */ }
          reject(new DOMException("Aborted", "AbortError"));
        };
        const onOpen = () => {
          traceP2P("Host", "peer-open", { roomCode, peerId, attempt });
          signal?.removeEventListener("abort", onAbort);
          console.log("[P2P Host] registered on signaling server, code:", roomCode);
          peer.off("error", onError);
          resolve();
        };
        const onError = (err: Error & { type?: string }) => {
          traceP2P("Host", "peer-open-error", {
            peerId,
            attempt,
            type: err.type,
            message: err.message,
          });
          signal?.removeEventListener("abort", onAbort);
          peer.off("open", onOpen);
          try { peer.destroy(); } catch { /* best-effort */ }
          // Preserve the PeerJS error type on `.cause` so callers can
          // classify without parsing the message.
          const wrapped = new Error(`Failed to create room: ${err.message}`);
          Object.assign(wrapped, { cause: err, peerErrorType: err.type });
          reject(wrapped);
        };
        signal?.addEventListener("abort", onAbort, { once: true });
        peer.once("open", onOpen);
        peer.once("error", onError);
      });
      return peer;
    } catch (err) {
      const peerErrorType = (err as { peerErrorType?: string }).peerErrorType;
      const canRetry =
        allowUnavailableIdRetry
        && peerErrorType === "unavailable-id"
        && attempt < UNAVAILABLE_ID_RETRY_BACKOFF_MS.length;
      if (!canRetry) throw err;

      const delay = UNAVAILABLE_ID_RETRY_BACKOFF_MS[attempt];
      traceP2P("Host", "unavailable-id-retry", { peerId, attempt, delay });
      await new Promise<void>((resolve, reject) => {
        const t = setTimeout(resolve, delay);
        signal?.addEventListener("abort", () => {
          clearTimeout(t);
          reject(new DOMException("Aborted", "AbortError"));
        }, { once: true });
      });
    }
  }
  throw new Error(
    `Failed to create room on ${peerId}: peer ID remained unavailable after retries`,
  );
}

/**
 * Host creates a room and returns a subscription handle. The host adapter
 * subscribes via `onGuestConnected` to wrap each incoming guest in a
 * `PeerSession` and track per-guest lifecycle.
 *
 * A 120s "no one joined" lobby timeout is NOT enforced here — the host
 * adapter owns lobby lifecycle (e.g., for 3-4 player games it must wait for
 * multiple guests, and the appropriate timeout depends on `playerCount`).
 *
 * Returns a Promise so the caller can await the host being registered on the
 * signaling server before exposing the room code to guests.
 */
export async function hostRoom(
  signal?: AbortSignal,
  options: HostRoomOptions = {},
): Promise<HostResult> {
  const roomCode = options.preferredRoomCode ?? generateRoomCode();
  const peerId = PEER_ID_PREFIX + roomCode;
  const isResume = options.preferredRoomCode !== undefined;

  let destroyed = false;
  const guestHandlers = new Set<(conn: DataConnection) => void>();
  // Connections that arrived after `peer.open` but before the adapter
  // subscribed via `onGuestConnected`. The adapter's construction is
  // interleaved with `await broker.registerHost()` + `await wasm.initialize()`
  // in GameProvider, so a guest dialing the room code (from a direct paste
  // or a broker-lobby click) can open its `DataConnection` before any
  // handler exists. We hold those opened conns here and flush them on the
  // first subscribe so no inbound guest is silently dropped.
  const pendingConns: DataConnection[] = [];

  // Open the Peer, retrying on `unavailable-id` when resuming: the PeerJS
  // signaling server may still hold the previous registration for a few
  // seconds after the prior host's TCP drops. Only resume gets the retry
  // — fresh hosts generate random codes so the collision would be
  // unrecoverable anyway.
  const peer = await openHostPeer(peerId, roomCode, isResume, signal);
  traceP2P("Host", "peer-open-final", { peerId, roomCode });

  // Multi-fire connection handler: every guest gets wrapped on `open`.
  peer.on("connection", (conn) => {
    traceP2P("Host", "peer-connection", {
      peerId,
      connOpen: conn.open,
    });
    if (destroyed) {
      try { conn.close(); } catch { /* best-effort */ }
      return;
    }
    conn.on("open", () => {
      traceP2P("Host", "conn-open", {
        peerId,
        connOpen: conn.open,
      });
      void logSelectedIceCandidate("Host", conn);
      if (destroyed) {
        try { conn.close(); } catch { /* best-effort */ }
        return;
      }
      if (guestHandlers.size === 0) {
        pendingConns.push(conn);
        return;
      }
      for (const handler of guestHandlers) {
        handler(conn);
      }
    });
    conn.on("close", () => {
      traceP2P("Host", "conn-close", { peerId });
    });
    // Per-conn open errors are non-fatal: the parent Peer survives so other
    // guests remain connected. The PeerSession's own error handler will fire
    // for already-open connections.
    conn.on("error", (err) => {
      traceP2P("Host", "conn-error", { peerId, message: err.message });
      console.warn("[P2P Host] guest connection error (non-fatal):", err);
    });
  });

  // Top-level Peer errors: PeerJS surfaces transient issues here too. Only
  // FATAL errors should trigger destroy — transient ones are recoverable.
  peer.on("error", (err: Error & { type?: string }) => {
    const fatal = err.type === "browser-incompatible"
      || err.type === "invalid-id"
      || err.type === "invalid-key"
      || err.type === "unavailable-id"
      || err.type === "ssl-unavailable"
      || err.type === "server-error"
      || err.type === "socket-error"
      || err.type === "socket-closed";
    if (fatal) {
      traceP2P("Host", "peer-fatal-error", { peerId, type: err.type, message: err.message });
      console.error("[P2P Host] fatal Peer error, destroying:", err);
      destroyed = true;
      try { peer.destroy(); } catch { /* best-effort */ }
    } else {
      traceP2P("Host", "peer-nonfatal-error", { peerId, type: err.type, message: err.message });
      console.warn("[P2P Host] non-fatal Peer error:", err);
    }
  });

  return {
    roomCode,
    peerId,
    peer,
    onGuestConnected(handler) {
      const wasEmpty = guestHandlers.size === 0;
      guestHandlers.add(handler);
      if (wasEmpty && pendingConns.length > 0) {
        const flush = pendingConns.splice(0);
        for (const conn of flush) handler(conn);
      }
      return () => {
        guestHandlers.delete(handler);
      };
    },
    destroy() {
      destroyed = true;
      guestHandlers.clear();
      try { peer.destroy(); } catch { /* best-effort */ }
    },
  };
}

/**
 * Guest joins a room by code. Returns the `Peer` separately from the
 * `DataConnection` so the guest adapter can keep the `Peer` alive across
 * `DataConnection` drops and attempt auto-reconnect via
 * `peer.connect(hostPeerId)`.
 */
export function joinRoom(code: string, signal?: AbortSignal, timeoutMs = 30_000): Promise<JoinResult> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(new DOMException("Aborted", "AbortError"));
      return;
    }
    const peer = new Peer({ config: PEER_CONFIG });
    const peerId = PEER_ID_PREFIX + code;
    let opened = false;
    traceP2P("Guest", "create-peer", { code, peerId });

    const onAbort = () => {
      if (opened) return;
      traceP2P("Guest", "abort-before-open", { peerId });
      try { peer.destroy(); } catch { /* best-effort */ }
      reject(new DOMException("Aborted", "AbortError"));
    };
    signal?.addEventListener("abort", onAbort, { once: true });

    peer.on("open", () => {
      if (signal?.aborted) {
        try { peer.destroy(); } catch { /* best-effort */ }
        return;
      }
      traceP2P("Guest", "peer-open", { peerId });
      console.log("[P2P Guest] registered on signaling server, connecting to:", peerId);
      // `serialization: "binary"` switches PeerJS to its MsgPackBinaryConnection,
      // which packs our `Uint8Array` wire bytes through BinaryPack (~3–6 byte
      // msgpack envelope) and retains BinaryPack's `_sendChunks` chunker for
      // SCTP fragmentation (max 16,300 B per frame). Required so we can send
      // gzip-compressed `encodeWireMessage` payloads over the channel.
      // The option lives on `PeerConnectOption`, not `PeerOptions`; host adopts
      // whatever the guest declares (verified at peerjs/bundler.mjs:1597).
      const conn = peer.connect(peerId, { serialization: "binary", reliable: true });
      traceP2P("Guest", "connect-called", { peerId, connOpen: conn.open });

      const timeout = setTimeout(() => {
        traceP2P("Guest", "connect-timeout", { peerId });
        signal?.removeEventListener("abort", onAbort);
        reject(new Error("Connection timed out. Check the room code and try again."));
        peer.destroy();
      }, timeoutMs);

      conn.on("open", () => {
        traceP2P("Guest", "conn-open", { peerId, connOpen: conn.open });
        void logSelectedIceCandidate("Guest", conn);
        clearTimeout(timeout);
        signal?.removeEventListener("abort", onAbort);
        opened = true;
        resolve({
          conn,
          peer,
          closeConn: () => {
            try { conn.close(); } catch { /* best-effort */ }
          },
          destroyPeer: () => {
            try { peer.destroy(); } catch { /* best-effort */ }
          },
        });
      });
      conn.on("close", () => {
        traceP2P("Guest", "conn-close", { peerId, opened });
      });

      conn.on("error", (err) => {
        traceP2P("Guest", "conn-error", { peerId, opened, message: err.message });
        if (opened) {
          console.warn("[P2P Guest] post-open connection error (non-fatal):", err);
          return;
        }
        clearTimeout(timeout);
        signal?.removeEventListener("abort", onAbort);
        reject(new Error(`Connection error: ${err.message}`));
        peer.destroy();
      });
    });

    // PeerJS emits connection failures on the peer, not the conn (issue #1281).
    // Mirror the host's classifier: post-open, only fatal types destroy the
    // Peer. The same fatal set applies on both sides of the signaling server.
    peer.on("error", (err: Error & { type?: string }) => {
      const fatal = err.type === "browser-incompatible"
        || err.type === "invalid-id"
        || err.type === "invalid-key"
        || err.type === "unavailable-id"
        || err.type === "ssl-unavailable"
        || err.type === "server-error"
        || err.type === "socket-error"
        || err.type === "socket-closed";
      if (!opened) {
        traceP2P("Guest", "peer-preopen-error", { peerId, type: err.type, message: err.message });
        // Pre-open: any peer error means the initial connect failed — reject.
        reject(new Error(`Failed to connect: ${err.message}`));
        try { peer.destroy(); } catch { /* best-effort */ }
        return;
      }
      if (fatal) {
        traceP2P("Guest", "peer-fatal-error", { peerId, type: err.type, message: err.message });
        console.error("[P2P Guest] fatal Peer error, destroying:", err);
        try { peer.destroy(); } catch { /* best-effort */ }
      } else {
        traceP2P("Guest", "peer-nonfatal-error", { peerId, type: err.type, message: err.message });
        console.warn("[P2P Guest] non-fatal Peer error (Peer kept alive for reconnect):", err);
      }
    });
  });
}
