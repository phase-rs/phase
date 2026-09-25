import { diagnosticIdFor, recordDiagnostic } from "../services/troubleshooting";
import type { ConnectionDiagnosticError, ConnectionFailureSnapshot, PeerDiagnosticError, TurnCredentialFailure } from "../services/troubleshooting";
import { createPeer, peerTransportFactory } from "./transport";
import type { PeerTransportFactory, TransportConnectOptions, TransportConnection, TransportPeer } from "./transport";

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

/**
 * The options **every** `peer.connect(...)` dial in the app must pass. Single
 * authority — a dial that omits these is a defect, not a shortcut.
 *
 * - `serialization: "binary"` selects PeerJS's BinaryPack connection, which
 *   packs our `Uint8Array` wire bytes through BinaryPack and applies its
 *   `_sendChunks` chunker for SCTP fragmentation (max 16,300 B per frame) —
 *   what carries our gzip-compressed `encodeWireMessage` payloads.
 *   It is stated **explicitly rather than because it changes anything**:
 *   `bundler.mjs:1463-1468` maps `binary`, `binary-utf8` AND `default` to that
 *   same class, and `connect()` already defaults to `"default"`
 *   (`bundler.mjs:1655-1658`), so passing it is a no-op documenting intent.
 *   (It is NOT MsgPack — that is a separate class at `bundler.mjs:1851`,
 *   reachable only via `options.serializers`.) The corollary matters for the
 *   bug this constant fixes: the two dials that previously passed no options
 *   were never on a different wire format, so the defect was ordering-only.
 * - `reliable: true` is the option that actually changes behaviour — PeerJS
 *   maps it to the data channel's **ordering**
 *   guarantee: the originator stores it (`bundler.mjs:1160`) and the negotiator
 *   passes it straight through as `createDataChannel(label, { ordered:
 *   !!options.reliable })` (`bundler.mjs:741-743`). Omit it and the channel is
 *   built UNORDERED — harmless on a direct path where reordering is rare, but
 *   routinely wrong over a TURN relay, and silently assumed by every downstream
 *   revision guard (which compares revisions and drops anything that looks
 *   stale, so a reordered frame is *discarded*, not resequenced).
 *
 * The options live on `PeerConnectOption`, not `PeerOptions`; the host adopts
 * whatever the dialing guest declares (verified at `peerjs/bundler.mjs:1597`).
 */
export const PEER_CONNECT_OPTIONS: TransportConnectOptions = {
  serialization: "binary",
  reliable: true,
};

/**
 * Budget for a first-contact dial (`joinRoom`). A relayed ICE negotiation —
 * TURN allocation, candidate exchange, connectivity checks — routinely needs
 * well over the ~2s a direct path takes, so this is deliberately generous.
 *
 * It does NOT slow down feedback for a mistyped room code: a nonexistent peer
 * fails immediately via `peer-unavailable`, which the pre-open `peer.on("error")`
 * handler below rejects on. This budget only applies once the peer exists and
 * ICE is still negotiating.
 */
export const JOIN_CONNECT_TIMEOUT_MS = 30_000;

/**
 * Budget for a *reconnect* dial, shared by the game guest and the draft guest.
 * Deliberately shorter than {@link JOIN_CONNECT_TIMEOUT_MS}, and sized against
 * the tighter of the two consumers: the GAME host renders a
 * `DisconnectChoiceDialog` counting down `DEFAULT_GRACE_PERIOD_MS` (30s), and a
 * 30s reconnect dial would let attempt 1 consume the entire visible window.
 * (The draft host's window is `DRAFT_GRACE_PERIOD_MS`, 60s, with no such
 * dialog — it is the looser constraint, so it does not bind this value.)
 * Not an auto-concede risk — the host arms
 * `timer: null` and the dialog's expiry path dismisses without conceding — the
 * cost is host *visibility*: the dialog closes before the guest's first attempt
 * even finishes. With `RECONNECT_BACKOFF_MS = [1000, 2000, 4000, 8000, …]` a 15s
 * budget puts attempt 1's completion at ~16s and attempt 2's at ~33s, i.e. one
 * completed attempt plus one in flight inside the 30s dialog.
 *
 * Note this stretches the reconnect cadence generally: `RECONNECT_BACKOFF_MS`
 * schedules the *gap between* attempts and was written when each attempt cost at
 * most the old 10s budget, so total elapsed time per attempt grows by 5s.
 */
export const RECONNECT_DIAL_TIMEOUT_MS = 15_000;

// ICE configuration. PeerJS's bundled TURN servers are broken, so we always
// supply our own. Credentials are minted on demand (short-lived) by the lobby
// Worker's /turn-credentials endpoint rather than hardcoded in the bundle —
// previously static Metered credentials shipped in plaintext and could be
// extracted to burn the relay quota.
/** Build-time override for operators who mint TURN credentials themselves. */
export const TURN_CREDENTIALS_URL = __TURN_CREDENTIALS_URL__;

// Used when the credentials endpoint is unreachable or unconfigured. STUN-only:
// direct and STUN-assisted connections still work; symmetric-NAT/CGNAT peers
// can't relay until /turn-credentials is live.
const FALLBACK_ICE_CONFIG: RTCConfiguration = {
  iceServers: [{ urls: "stun:stun.cloudflare.com:3478" }],
};

// Cache the fetched config well inside the credential TTL (server mints 24h
// creds) so we don't refetch on every host/join/reconnect attempt.
const ICE_CONFIG_CACHE_MS = 6 * 60 * 60 * 1000;
let cachedIceConfig: { config: RTCConfiguration; expiresAt: number } | null = null;

export class TurnCredentialError extends Error {
  constructor(readonly reason: TurnCredentialFailure, readonly httpStatus?: number) {
    super(`TURN credentials: ${reason}`);
  }
}

/** Strict, uncached retrieval for both game setup and isolated diagnostics. */
export async function fetchFreshTurnConfig(signal?: AbortSignal): Promise<RTCConfiguration> {
  try {
    if (signal?.aborted) throw new TurnCredentialError("aborted");
    const response = await fetch(TURN_CREDENTIALS_URL, {
      signal, cache: "no-store", credentials: "omit", referrerPolicy: "no-referrer",
    });
    if (!response.ok) throw new TurnCredentialError("http", response.status);
    let data: unknown;
    try { data = await response.json(); }
    catch { throw new TurnCredentialError(signal?.aborted ? "aborted" : "invalid"); }
    if (signal?.aborted) throw new TurnCredentialError("aborted");
    if (!data || typeof data !== "object" || !("iceServers" in data) || !Array.isArray(data.iceServers)) {
      throw new TurnCredentialError("invalid");
    }
    const iceServers: RTCIceServer[] = [];
    let hasTurn = false;
    for (const server of data.iceServers) {
      if (!server || typeof server !== "object") throw new TurnCredentialError("invalid");
      const urls: unknown[] = typeof server.urls === "string" ? [server.urls] : server.urls;
      if (!Array.isArray(urls) || !urls.length || !urls.every((url) => typeof url === "string" && /^(stun|stuns|turn|turns):[^\s]+$/i.test(url))) throw new TurnCredentialError("invalid");
      const relay = urls.some((url) => /^turns?:/i.test(url as string));
      if (relay && (typeof server.username !== "string" || !server.username || typeof server.credential !== "string" || !server.credential)) throw new TurnCredentialError("invalid");
      hasTurn ||= relay;
      iceServers.push({ urls: urls as string[], ...(relay ? { username: server.username, credential: server.credential } : {}) });
    }
    if (!hasTurn) throw new TurnCredentialError("no-turn");
    return { iceServers };
  } catch (error) {
    if (error instanceof TurnCredentialError) throw error;
    throw new TurnCredentialError(signal?.aborted ? "aborted" : "network");
  }
}

async function getPeerConfig(): Promise<RTCConfiguration> {
  const now = Date.now();
  if (cachedIceConfig && cachedIceConfig.expiresAt > now) {
    recordDiagnostic({ kind: "credentials", observedAt: now, outcome: "cache" });
    return cachedIceConfig.config;
  }
  try {
    const config = await fetchFreshTurnConfig();
    cachedIceConfig = { config, expiresAt: now + ICE_CONFIG_CACHE_MS };
    recordDiagnostic({ kind: "credentials", observedAt: Date.now(), outcome: "fresh" });
    return config;
  } catch (error) {
    recordDiagnostic({ kind: "credentials", observedAt: Date.now(), outcome: "stun-fallback",
      failure: error instanceof TurnCredentialError ? error.reason : "network",
      ...(error instanceof TurnCredentialError && error.httpStatus !== undefined ? { httpStatus: error.httpStatus } : {}),
    });
    return FALLBACK_ICE_CONFIG;
  }
}

export function safePeerError(error: unknown): PeerDiagnosticError {
  const type = error && typeof error === "object" && "type" in error ? error.type : null;
  switch (type) {
    case "browser-incompatible": case "disconnected": case "invalid-id": case "invalid-key":
    case "network": case "peer-unavailable": case "ssl-unavailable": case "server-error":
    case "socket-error": case "socket-closed": case "unavailable-id": case "webrtc": return type;
    default: return "unknown";
  }
}

export function safeConnectionError(error: unknown): ConnectionDiagnosticError {
  const type = error && typeof error === "object" && "type" in error ? error.type : null;
  switch (type) {
    case "negotiation-failed": case "connection-closed": case "message-too-big": return type;
    default: return "unknown";
  }
}

export function connectionFailureSnapshot(conn: TransportConnection): ConnectionFailureSnapshot {
  return { connectionState: conn.peerConnection?.connectionState ?? null,
    iceState: conn.peerConnection?.iceConnectionState ?? null, channelState: conn.dataChannel?.readyState ?? null };
}

/** Observe the public emitter before registration, including failed registration. */
function createObservedPeer(
  side: "Host" | "Guest",
  config: RTCConfiguration,
  id?: string,
  transportFactory: PeerTransportFactory = peerTransportFactory,
): TransportPeer {
  const identity = {};
  let peerDiagnosticId = diagnosticIdFor(identity);
  const record = (event: "created" | "open" | "disconnected" | "close" | "timeout" | "error" | "constructor-error", error?: PeerDiagnosticError) => {
    recordDiagnostic({ kind: "signaling", peerDiagnosticId, observedAt: Date.now(), side, event, ...(error ? { error } : {}) });
  };
  let peer: TransportPeer;
  try { peer = createPeer(id, { config }, transportFactory); }
  catch (error) { record("constructor-error", safePeerError(error)); throw error; }
  peerDiagnosticId = diagnosticIdFor(peer);
  record("created");
  // Observation only: leave registration cancellation/retry policy with its owner.
  const timer = setTimeout(() => record("timeout"), JOIN_CONNECT_TIMEOUT_MS);
  peer.on("open", () => { clearTimeout(timer); record("open"); });
  peer.on("disconnected", () => record("disconnected"));
  peer.on("error", (error) => { clearTimeout(timer); record("error", safePeerError(error)); });
  peer.on("close", () => { clearTimeout(timer); record("close"); });
  peer.on("connection", (conn) => observeConnectionAttempt(conn, "incoming", JOIN_CONNECT_TIMEOUT_MS, peer));
  return peer;
}

function observeConnectionAttempt(conn: TransportConnection, direction: "incoming" | "outgoing", timeoutMs: number, peer: TransportPeer, signal?: AbortSignal): void {
  const diagnosticId = diagnosticIdFor(conn);
  const peerDiagnosticId = diagnosticIdFor(peer);
  const record = (event: "started" | "open" | "close" | "error" | "timeout" | "aborted", error?: ConnectionDiagnosticError) => recordDiagnostic({ kind: "connection-attempt", diagnosticId, peerDiagnosticId, observedAt: Date.now(), direction, event, ...(error ? { error } : {}), state: connectionFailureSnapshot(conn) });
  record("started");
  const pc = conn.peerConnection;
  const onIceError = (event: RTCPeerConnectionIceErrorEvent) => {
    if (Number.isInteger(event.errorCode) && event.errorCode >= 300 && event.errorCode <= 799) recordDiagnostic({ kind: "ice-candidate-error", diagnosticId, peerDiagnosticId, observedAt: Date.now(), code: event.errorCode });
  };
  pc?.addEventListener?.("icecandidateerror", onIceError);
  let finished = false;
  const finish = (event: "open" | "close" | "error" | "timeout" | "aborted", error?: ConnectionDiagnosticError) => {
    if (finished) return;
    finished = true;
    clearTimeout(timer);
    pc?.removeEventListener?.("icecandidateerror", onIceError);
    signal?.removeEventListener("abort", onAbort);
    peer.off?.("close", onPeerClose);
    record(event, error);
  };
  const onAbort = () => finish("aborted");
  const onPeerClose = () => finish(signal?.aborted ? "aborted" : "close");
  const timer = setTimeout(() => finish("timeout"), timeoutMs);
  signal?.addEventListener("abort", onAbort, { once: true });
  peer.on?.("close", onPeerClose);
  conn.on("open", () => finish("open"));
  conn.on("error", (error) => finish("error", safeConnectionError(error)));
  conn.on("close", () => finish(signal?.aborted ? "aborted" : "close"));
  if (signal?.aborted) onAbort();
}

/** Every outgoing connection uses the same ordering and serialization contract. */
export function dialPeer(peer: TransportPeer, peerId: string, timeoutMs: number, signal?: AbortSignal): TransportConnection {
  try {
    const conn = peer.connect(peerId, PEER_CONNECT_OPTIONS);
    if (!conn) throw new Error("Peer connection could not be created");
    observeConnectionAttempt(conn, "outgoing", timeoutMs, peer, signal);
    return conn;
  } catch (error) {
    recordDiagnostic({ kind: "connection-attempt", diagnosticId: diagnosticIdFor({}), peerDiagnosticId: diagnosticIdFor(peer), observedAt: Date.now(), direction: "outgoing", event: "error", error: safeConnectionError(error) });
    throw error;
  }
}

function traceP2P(side: "Host" | "Guest", event: string, data?: Record<string, unknown>): void {
  console.debug(`[P2P ${side} Trace]`, performance.now().toFixed(1), event, data ?? {});
}

/** Restore signaling without tearing down established WebRTC connections.
 * PeerJS retains those connections on `disconnected`; `destroy()` does not.
 * Use its reconnect API with bounded backoff until recovery or owner teardown. */
function maintainSignaling(peer: TransportPeer): void {
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  let retryDelay = 1000;
  const clearRetry = () => {
    if (retryTimer !== null) clearTimeout(retryTimer);
    retryTimer = null;
  };
  peer.on("disconnected", () => {
    if (peer.destroyed || retryTimer !== null) return;
    retryTimer = setTimeout(() => {
      retryTimer = null;
      if (!peer.destroyed && peer.disconnected) peer.reconnect();
    }, retryDelay);
    retryDelay = Math.min(retryDelay * 2, 30_000);
  });
  peer.on("open", () => {
    clearRetry();
    retryDelay = 1000;
  });
  peer.on("close", clearRetry);
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
  peerConnection?: Pick<RTCPeerConnection, "getStats"> | null | undefined;
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
  peer: TransportPeer;
  /**
   * Subscribe to incoming guest connections. Multi-fire: handler is called for
   * every new guest after their `DataConnection.open` event. Returns an
   * unsubscribe function.
   *
   * Each `DataConnection` is delivered to the host adapter, which is
   * responsible for wrapping it in a `PeerSession` (with its own
   * `onSessionEnd` callback) and tracking the per-guest lifecycle.
   */
  onGuestConnected: (handler: (conn: TransportConnection) => void) => () => void;
  /**
   * Tear down the shared `Peer`. Sole authoritative cleanup site for the
   * underlying signaling-server connection. Per-session disconnects must NOT
   * call this — that would cascade-kill all sibling guests.
   */
  destroy: () => void;
}

export interface JoinResult {
  conn: TransportConnection;
  peer: TransportPeer;
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
  /** Transport construction dependency for this host session. */
  transportFactory?: PeerTransportFactory;
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
  transportFactory: PeerTransportFactory = peerTransportFactory,
): Promise<TransportPeer> {
  const maxAttempts = allowUnavailableIdRetry
    ? UNAVAILABLE_ID_RETRY_BACKOFF_MS.length + 1
    : 1;

  // Fetch ICE config once up front so all retry attempts reuse it (and we don't
  // hit the credentials endpoint per attempt).
  const config = await getPeerConfig();

  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    if (signal?.aborted) throw new DOMException("Aborted", "AbortError");

    const peer = createObservedPeer("Host", config, peerId, transportFactory);
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
  const guestHandlers = new Set<(conn: TransportConnection) => void>();
  // Connections that arrived after `peer.open` but before the adapter
  // subscribed via `onGuestConnected`. The adapter's construction is
  // interleaved with `await broker.registerHost()` + `await wasm.initialize()`
  // in GameProvider, so a guest dialing the room code (from a direct paste
  // or a broker-lobby click) can open its `DataConnection` before any
  // handler exists. We hold those opened conns here and flush them on the
  // first subscribe so no inbound guest is silently dropped.
  const pendingConns: TransportConnection[] = [];

  // Open the Peer, retrying on `unavailable-id` when resuming: the PeerJS
  // signaling server may still hold the previous registration for a few
  // seconds after the prior host's TCP drops. Only resume gets the retry
  // — fresh hosts generate random codes so the collision would be
  // unrecoverable anyway.
  const peer = await openHostPeer(peerId, roomCode, isResume, signal, options.transportFactory);
  maintainSignaling(peer);
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

  // PeerJS owns error teardown. Once registered, its abort path disconnects
  // signaling while preserving DataConnections. Calling destroy here would
  // turn a signaling outage into a disconnect for every guest in the game.
  peer.on("error", (err: Error & { type?: string }) => {
    traceP2P("Host", "peer-error", { peerId, type: err.type, message: err.message });
    console.warn("[P2P Host] Peer error (existing connections preserved):", err);
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
 * `peer.connect(hostPeerId, PEER_CONNECT_OPTIONS)`.
 */
export async function joinRoom(
  code: string,
  signal?: AbortSignal,
  timeoutMs = JOIN_CONNECT_TIMEOUT_MS,
  transportFactory: PeerTransportFactory = peerTransportFactory,
): Promise<JoinResult> {
  if (signal?.aborted) throw new DOMException("Aborted", "AbortError");
  const config = await getPeerConfig();
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(new DOMException("Aborted", "AbortError"));
      return;
    }
    const peer = createObservedPeer("Guest", config, undefined, transportFactory);
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

    // A signaling reconnect also emits open; only the initial registration
    // should dial the host. The adapter owns subsequent game-channel dials.
    peer.once("open", () => {
      if (signal?.aborted) {
        try { peer.destroy(); } catch { /* best-effort */ }
        return;
      }
      traceP2P("Guest", "peer-open", { peerId });
      console.log("[P2P Guest] registered on signaling server, connecting to:", peerId);
      const conn = dialPeer(peer, peerId, timeoutMs, signal);
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
        maintainSignaling(peer);
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
    // Before the initial game connection opens, reject a failed join. After
    // that, preserve existing channels and let PeerJS manage signaling loss.
    peer.on("error", (err: Error & { type?: string }) => {
      if (!opened) {
        traceP2P("Guest", "peer-preopen-error", { peerId, type: err.type, message: err.message });
        // Pre-open: any peer error means the initial connect failed — reject.
        reject(new Error(`Failed to connect: ${err.message}`));
        try { peer.destroy(); } catch { /* best-effort */ }
        return;
      }
      traceP2P("Guest", "peer-error", { peerId, type: err.type, message: err.message });
      console.warn("[P2P Guest] Peer error (existing connections preserved):", err);
    });
  });
}
