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

import type { PoolInput } from "../adapter/draft-adapter";
import { ACTIVE_DRAFT_POD_KEY } from "../constants/storage";

export type { PoolInput } from "../adapter/draft-adapter";

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
  tournamentFormat: "Swiss" | "SingleElimination";
  podPolicy: "Competitive" | "Casual";
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
  /** Pool source for re-initialization on resume (Set pool JSON or Cube list + settings). */
  poolInput: PoolInput;
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

export type ActiveDraftPodPhase =
  | "lobby"
  | "drafting"
  | "deckbuilding"
  | "pairing"
  | "matchInProgress"
  | "complete";

export interface ActiveDraftPodMeta {
  id: string;
  roomCode: string;
  kind: "Premier" | "Traditional";
  podSize: number;
  hostDisplayName: string;
  tournamentFormat: "Swiss" | "SingleElimination";
  podPolicy: "Competitive" | "Casual";
  phase: ActiveDraftPodPhase;
  pickCount: number;
  updatedAt: number;
}

// ── Store ──────────────────────────────────────────────────────────────

const DRAFT_HOST_PREFIX = "phase-draft-host:";
const DRAFT_GUEST_PREFIX = "phase-draft-guest:";
const HOST_SESSION_TTL_MS = 24 * 60 * 60 * 1000;
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
    if (!s) return null;
    // C6 shape guard: legacy snapshots (pre-#1253) carried a flat
    // `setPoolJson: string` field instead of the PoolInput discriminated
    // union. Discriminate on the new shape; reject anything that doesn't
    // self-identify as Set or Cube so the resume path falls back to
    // "no draft pod to resume" instead of crashing on `persisted.poolInput.data`.
    if (s.poolInput?.type !== "Set" && s.poolInput?.type !== "Cube") {
      return null;
    }
    return s;
  } catch {
    return null;
  }
}

export async function clearDraftHostSession(id: string): Promise<void> {
  try {
    await del(DRAFT_HOST_PREFIX + id, getDraftStore());
  } catch { /* best-effort */ }
}

// ── Active Host Meta ──────────────────────────────────────────────────

export function saveActiveDraftPod(meta: ActiveDraftPodMeta): void {
  localStorage.setItem(ACTIVE_DRAFT_POD_KEY, JSON.stringify(meta));
}

export function loadActiveDraftPod(): ActiveDraftPodMeta | null {
  try {
    const raw = localStorage.getItem(ACTIVE_DRAFT_POD_KEY);
    if (!raw) return null;
    const meta = JSON.parse(raw) as ActiveDraftPodMeta;
    if (Date.now() - meta.updatedAt > HOST_SESSION_TTL_MS) {
      void clearDraftHostSession(meta.id);
      clearActiveDraftPod();
      return null;
    }
    return meta;
  } catch {
    return null;
  }
}

export function clearActiveDraftPod(): void {
  localStorage.removeItem(ACTIVE_DRAFT_POD_KEY);
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
