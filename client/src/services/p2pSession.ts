/**
 * Persistent session token storage for P2P games.
 *
 * Tokens are issued by the host on `game_setup` / `reconnect_ack` and consumed
 * by the guest on auto-reconnect. Persisting to IndexedDB (not sessionStorage)
 * means a guest whose tab crashed or was accidentally closed can reopen
 * and still rejoin their original seat — the host recognizes the token and
 * rebinds the PlayerId through `handleReconnect`.
 *
 * Pre-game tokens (issued on lobby join but before `game_setup`) are
 * intentionally NOT persisted — a guest who drops during the lobby must
 * rejoin fresh.
 */

import { createStore, del, get, set } from "idb-keyval";

const STORAGE_PREFIX = "phase-p2p-session:";
const HOST_LEASE_PREFIX = "phase-p2p-host-lease:";
const inMemoryHostLeases = new Map<P2PSessionKey, string>();
/**
 * How long a persisted guest token is valid. Sized for host-resume
 * realism: a host who crashed, reopened the tab, and dialed back in on
 * the same room code within 4 hours can still rejoin guests. Beyond
 * that window, the token is considered stale and the guest rejoins
 * fresh (new seat if lobby, rejected if mid-game).
 */
const SESSION_TTL_MS = 4 * 60 * 60 * 1000;

export interface P2PSessionData {
  hostPeerId: string;
  playerToken: string;
  playerId: number;
  /** Stable identity of the hosted game, retained across host resume. */
  authority: P2PAuthorityStamp;
  timestamp: number;
}

/**
 * Stable, opaque identity for one hosted P2P game. It is deliberately
 * independent of a PeerJS id: a resumed host may reclaim the room code while
 * receiving a new PeerJS process/incarnation.
 */
export type P2PSessionKey = string;

/**
 * A host lease is a fencing token. A new host resuming the same session key
 * replaces the incarnation synchronously; any older host must check this
 * stamp before it emits a frame, changes authority state, or persists.
 */
export interface P2PAuthorityStamp {
  sessionKey: P2PSessionKey;
  hostIncarnation: string;
}

/** Validates the exact persisted/wire authority envelope. */
export function isP2PAuthorityStamp(value: unknown): value is P2PAuthorityStamp {
  if (value == null || typeof value !== "object" || Array.isArray(value)) return false;
  const authority = value as Record<string, unknown>;
  const keys = Object.keys(authority);
  return keys.length === 2
    && keys.includes("sessionKey")
    && keys.includes("hostIncarnation")
    && typeof authority.sessionKey === "string"
    && typeof authority.hostIncarnation === "string";
}

/**
 * Checks the full fencing token, not just the stable session key. A guest may
 * reconnect to a resumed host under the same session key, but every normal
 * post-handshake frame must come from the exact host incarnation it adopted.
 */
export function hasExactP2PAuthority(
  candidate: P2PAuthorityStamp | undefined,
  authority: P2PAuthorityStamp,
): boolean {
  return isP2PAuthorityStamp(candidate)
    && candidate.sessionKey === authority.sessionKey
    && candidate.hostIncarnation === authority.hostIncarnation;
}

export function createP2PSessionKey(): P2PSessionKey {
  return crypto.randomUUID();
}

function leaseStorageKey(sessionKey: P2PSessionKey): string {
  return HOST_LEASE_PREFIX + sessionKey;
}

function readHostLease(sessionKey: P2PSessionKey): P2PAuthorityStamp | null {
  try {
    const raw = localStorage.getItem(leaseStorageKey(sessionKey));
    if (!raw) return null;
    const stamp = JSON.parse(raw) as Partial<P2PAuthorityStamp>;
    return stamp.sessionKey === sessionKey && typeof stamp.hostIncarnation === "string"
      ? { sessionKey, hostIncarnation: stamp.hostIncarnation }
      : null;
  } catch {
    return null;
  }
}

/** Claim the sole local authority lease for a stable P2P session. */
export function claimP2PHostLease(sessionKey: P2PSessionKey): P2PAuthorityStamp {
  const authority = { sessionKey, hostIncarnation: crypto.randomUUID() };
  inMemoryHostLeases.set(sessionKey, authority.hostIncarnation);
  try {
    localStorage.setItem(leaseStorageKey(sessionKey), JSON.stringify(authority));
  } catch (err) {
    // The in-memory stamp still fences this adapter's frames. Storage failure
    // only removes cross-tab resume fencing, and must not prevent hosting.
    console.warn("[p2pSession] host lease write failed:", err);
  }
  return authority;
}

export function ownsP2PHostLease(authority: P2PAuthorityStamp): boolean {
  const current = readHostLease(authority.sessionKey);
  if (current) return current.hostIncarnation === authority.hostIncarnation;
  // Storage can be unavailable (private mode, quota policy). The module-local
  // fallback preserves same-tab fencing; it intentionally does not revive an
  // older host after the current incarnation has released its lease.
  return inMemoryHostLeases.get(authority.sessionKey) === authority.hostIncarnation;
}

/** Release only the caller's incarnation; never erase a newer resumed host. */
export function releaseP2PHostLease(authority: P2PAuthorityStamp): void {
  if (!ownsP2PHostLease(authority)) return;
  inMemoryHostLeases.delete(authority.sessionKey);
  try {
    localStorage.removeItem(leaseStorageKey(authority.sessionKey));
  } catch {
    /* best-effort */
  }
}

let _store: ReturnType<typeof createStore> | undefined;

function getSessionStore(): ReturnType<typeof createStore> {
  if (!_store) {
    _store = createStore("phase-p2p-session", "phase-p2p-session");
  }
  return _store;
}

function storageKey(hostPeerId: string): string {
  return STORAGE_PREFIX + hostPeerId;
}

function isFresh(session: P2PSessionData): boolean {
  return Date.now() - session.timestamp < SESSION_TTL_MS;
}

export async function saveP2PSession(
  hostPeerId: string,
  data: { playerToken: string; playerId: number; authority: P2PAuthorityStamp },
): Promise<void> {
  const session: P2PSessionData = {
    hostPeerId,
    playerToken: data.playerToken,
    playerId: data.playerId,
    authority: data.authority,
    timestamp: Date.now(),
  };
  try {
    await set(storageKey(hostPeerId), session, getSessionStore());
  } catch (err) {
    console.warn("[p2pSession] IDB write failed:", err);
  }
}

export async function loadP2PSession(hostPeerId: string): Promise<P2PSessionData | null> {
  try {
    const session = await get<P2PSessionData>(storageKey(hostPeerId), getSessionStore());
    if (!session) return null;
    if (!isFresh(session) || !isP2PAuthorityStamp(session.authority)) {
      await clearP2PSession(hostPeerId);
      return null;
    }
    return session;
  } catch {
    return null;
  }
}

export async function clearP2PSession(hostPeerId: string): Promise<void> {
  try {
    await del(storageKey(hostPeerId), getSessionStore());
  } catch { /* best-effort */ }
}
