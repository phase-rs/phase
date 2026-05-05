/**
 * Draft-specific persistence for P2P tournament sessions.
 *
 * Separate from `gamePersistence.ts` (which handles engine GameState)
 * because draft session data has a different shape and lifecycle:
 * - Host persists the full DraftSession JSON + seat tokens after every mutation (P2P-05)
 * - Guest persists the draft token at pod join time (P2P-04)
 *
 * Both use IndexedDB via idb-keyval for the same reasons as game persistence:
 * draft sessions can be large (8 players x 42 cards each = significant JSON).
 */

import { createStore, del, get, set } from "idb-keyval";

// ── Types ──────────────────────────────────────────────────────────────

/**
 * Persisted snapshot of a P2P draft host session.
 *
 * Written after every authoritative mutation (guest join, pick, deck submit,
 * kick) so a crashed/reloaded host can restore the draft pod.
 */
export interface PersistedDraftHostSession {
  persistenceId: string;
  roomCode: string;
  kind: "Premier" | "Traditional";
  podSize: number;
  hostDisplayName: string;
  /** Seat index -> token. */
  seatTokens: Record<number, string>;
  /** Seat index -> display name. */
  seatNames: Record<number, string>;
  /** Tokens that were kicked — refused on reconnect. */
  kickedTokens: string[];
  /** Whether StartDraft has been applied. */
  draftStarted: boolean;
  /** Draft code for display/identification. */
  draftCode: string;
  /** Serialized DraftSession JSON from draft-wasm. Null if draft hasn't started. */
  draftSessionJson: string | null;
  /** Original set pool JSON for re-initialization on resume. */
  setPoolJson: string;
}

/**
 * Persisted guest token for draft reconnection.
 *
 * Saved at pod join time (P2P-04) so a guest whose tab crashes can
 * reopen and rejoin their seat.
 */
export interface PersistedDraftGuestSession {
  hostPeerId: string;
  draftToken: string;
  seatIndex: number;
  draftCode: string;
  timestamp: number;
}

// ── Store ──────────────────────────────────────────────────────────────

const DRAFT_HOST_PREFIX = "phase-draft-host:";
const DRAFT_GUEST_PREFIX = "phase-draft-guest:";
/** Guest token TTL — 4 hours matches the game session TTL. */
const GUEST_SESSION_TTL_MS = 4 * 60 * 60 * 1000;

let _store: ReturnType<typeof createStore> | undefined;

export function getDraftStore(): ReturnType<typeof createStore> {
  if (!_store) {
    _store = createStore("phase-draft-session", "phase-draft-session");
  }
  return _store;
}

// ── Host Persistence ───────────────────────────────────────────────────

export async function saveDraftHostSession(
  id: string,
  session: PersistedDraftHostSession,
): Promise<void> {
  try {
    await set(DRAFT_HOST_PREFIX + id, session, getDraftStore());
  } catch (err) {
    console.warn("[saveDraftHostSession] IDB write failed:", err);
  }
}

export async function loadDraftHostSession(
  id: string,
): Promise<PersistedDraftHostSession | null> {
  try {
    const s = await get<PersistedDraftHostSession>(
      DRAFT_HOST_PREFIX + id,
      getDraftStore(),
    );
    return s ?? null;
  } catch {
    return null;
  }
}

export async function clearDraftHostSession(id: string): Promise<void> {
  try {
    await del(DRAFT_HOST_PREFIX + id, getDraftStore());
  } catch { /* best-effort */ }
}

// ── Guest Persistence ──────────────────────────────────────────────────

export async function saveDraftGuestSession(
  hostPeerId: string,
  data: { draftToken: string; seatIndex: number; draftCode: string },
): Promise<void> {
  const session: PersistedDraftGuestSession = {
    hostPeerId,
    draftToken: data.draftToken,
    seatIndex: data.seatIndex,
    draftCode: data.draftCode,
    timestamp: Date.now(),
  };
  try {
    await set(DRAFT_GUEST_PREFIX + hostPeerId, session, getDraftStore());
  } catch (err) {
    console.warn("[saveDraftGuestSession] IDB write failed:", err);
  }
}

export async function loadDraftGuestSession(
  hostPeerId: string,
): Promise<PersistedDraftGuestSession | null> {
  try {
    const session = await get<PersistedDraftGuestSession>(
      DRAFT_GUEST_PREFIX + hostPeerId,
      getDraftStore(),
    );
    if (!session) return null;
    if (Date.now() - session.timestamp > GUEST_SESSION_TTL_MS) {
      await clearDraftGuestSession(hostPeerId);
      return null;
    }
    return session;
  } catch {
    return null;
  }
}

export async function clearDraftGuestSession(hostPeerId: string): Promise<void> {
  try {
    await del(DRAFT_GUEST_PREFIX + hostPeerId, getDraftStore());
  } catch { /* best-effort */ }
}
