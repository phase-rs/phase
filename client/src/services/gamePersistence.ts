import { createStore, del, get, set } from "idb-keyval";

import type { FormatConfig, GameState, MatchConfig } from "../adapter/types";
import type { SeatState } from "../multiplayer/seatTypes";
import { ACTIVE_GAME_KEY, GAME_CHECKPOINTS_PREFIX, GAME_KEY_PREFIX } from "../constants/storage";

export interface ActiveGameMeta {
  id: string;
  /**
   * `"online"` = server-mode WebSocket game. `"p2p-host"` = P2P host
   * game with local authoritative state; eligible for host-resume on
   * reload. P2P guests don't have an active meta — reconnection is
   * driven by the host's room code + the guest's persisted session
   * token.
   */
  mode: "ai" | "local" | "online" | "p2p-host";
  difficulty: string;
}

/**
 * Persistent snapshot of a P2P host session so a reloaded/crashed host
 * can resume the game on the same room code. Mirrors the server-side
 * `PersistedSession` pattern in `server-core::persist`:
 *
 * - `state: GameState` lives in a separate IDB record via `saveGame`
 *   (written on every action). This record is only written on
 *   lifecycle events (guest join, reconnect, game start, kick, elim).
 * - `playerTokens` is keyed by PlayerId numeric value so non-contiguous
 *   seats (e.g., pre-game disconnect + rejoin) round-trip correctly.
 * - `kickedTokens` and `eliminatedSeats` preserve security / semantic
 *   invariants across restart — without them, a kicked guest could
 *   reconnect on a resumed host, and a conceded guest could re-enter
 *   the seat the engine thinks is eliminated.
 */
export interface PersistedP2PHostSession {
  gameId: string;
  /** Bare 5-char room code; the PeerJS prefix is reattached by `hostRoom`. */
  roomCode: string;
  brokerGameCode?: string;
  useBroker: boolean;
  /** PlayerId.0 → token. PlayerId 0 is the host's own slot. */
  playerTokens: Record<number, string>;
  /** PlayerId.0 → deck submitted by that guest (pre-game data). */
  guestDecks: Record<number, unknown>;
  /** PlayerId.0 → resolved AI deck for AI-controlled seats. */
  aiDecks?: Record<number, unknown>;
  /** Tokens that were kicked — refused on reconnect on resume. */
  kickedTokens: string[];
  /** PlayerId.0 values that conceded. */
  eliminatedSeats: number[];
  playerCount: number;
  formatConfig?: FormatConfig;
  matchConfig?: MatchConfig;
  hostDeckData: unknown;
  /** True once `initializeGame` has run; false while still in lobby. */
  gameStarted: boolean;
  seatState?: SeatState;
}

const P2P_HOST_KEY_PREFIX = "phase-p2p-host:";

/**
 * Dedicated IndexedDB store for game state persistence.
 * Game state can easily exceed localStorage's ~5MB quota (120+ serialized
 * GameObjects with full ability definitions), so we use IndexedDB which
 * has no practical size limit.
 *
 * ActiveGameMeta remains in localStorage — it's < 200 bytes and benefits
 * from synchronous access for instant menu rendering.
 *
 * The IDB store is lazily created on first use to avoid errors in
 * environments where IndexedDB is unavailable (tests, SSR).
 */
let _gameStore: ReturnType<typeof createStore> | undefined;

function getGameStore(): ReturnType<typeof createStore> {
  if (!_gameStore) {
    _gameStore = createStore("phase-game-state", "phase-game-state");
  }
  return _gameStore;
}

// ── Game State (IndexedDB) ──────────────────────────────────────────────

export async function saveGame(gameId: string, state: GameState): Promise<void> {
  if (
    state.match_phase === "Completed"
    || (!state.match_phase && state.waiting_for.type === "GameOver")
  ) {
    await clearGame(gameId);
    return;
  }
  try {
    await set(GAME_KEY_PREFIX + gameId, state, getGameStore());
  } catch (err) {
    console.warn("[saveGame] IndexedDB write failed:", err);
  }
}

export async function loadGame(gameId: string): Promise<GameState | null> {
  try {
    const state = await get<GameState>(GAME_KEY_PREFIX + gameId, getGameStore());
    return state ?? null;
  } catch {
    return null;
  }
}

export async function clearGame(gameId: string): Promise<void> {
  try {
    await del(GAME_KEY_PREFIX + gameId, getGameStore());
    await del(GAME_CHECKPOINTS_PREFIX + gameId, getGameStore());
    // P2P host meta is scoped to the same gameId — a completed/reset game
    // must drop its resume metadata too, or the menu's Resume button
    // would surface a game the engine has forgotten.
    await del(P2P_HOST_KEY_PREFIX + gameId, getGameStore());
  } catch { /* best effort */ }
  const active = loadActiveGame();
  if (active?.id === gameId) {
    clearActiveGame();
  }
}

// ── P2P Host Session (IndexedDB) ────────────────────────────────────────

export async function saveP2PHostSession(
  gameId: string,
  session: PersistedP2PHostSession,
): Promise<void> {
  try {
    await set(P2P_HOST_KEY_PREFIX + gameId, session, getGameStore());
  } catch (err) {
    console.warn("[saveP2PHostSession] IndexedDB write failed:", err);
  }
}

export async function loadP2PHostSession(
  gameId: string,
): Promise<PersistedP2PHostSession | null> {
  try {
    const s = await get<PersistedP2PHostSession>(
      P2P_HOST_KEY_PREFIX + gameId,
      getGameStore(),
    );
    return s ?? null;
  } catch {
    return null;
  }
}

export async function clearP2PHostSession(gameId: string): Promise<void> {
  try {
    await del(P2P_HOST_KEY_PREFIX + gameId, getGameStore());
  } catch { /* best-effort */ }
}

// ── Checkpoints (IndexedDB) ─────────────────────────────────────────────

export async function saveCheckpoints(gameId: string, checkpoints: GameState[]): Promise<void> {
  try {
    await set(GAME_CHECKPOINTS_PREFIX + gameId, checkpoints, getGameStore());
  } catch { /* best effort */ }
}

export async function loadCheckpoints(gameId: string): Promise<GameState[]> {
  try {
    const checkpoints = await get<GameState[]>(GAME_CHECKPOINTS_PREFIX + gameId, getGameStore());
    return checkpoints ?? [];
  } catch {
    return [];
  }
}

// ── Active Game Meta (localStorage — tiny, synchronous) ─────────────────

export function saveActiveGame(meta: ActiveGameMeta): void {
  localStorage.setItem(ACTIVE_GAME_KEY, JSON.stringify(meta));
}

export function loadActiveGame(): ActiveGameMeta | null {
  try {
    const raw = localStorage.getItem(ACTIVE_GAME_KEY);
    if (!raw) return null;
    return JSON.parse(raw) as ActiveGameMeta;
  } catch {
    return null;
  }
}

export function clearActiveGame(): void {
  localStorage.removeItem(ACTIVE_GAME_KEY);
}
