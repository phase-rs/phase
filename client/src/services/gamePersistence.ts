import { createStore, del, get, set } from "idb-keyval";

import type { GameState } from "../adapter/types";
import { ACTIVE_GAME_KEY, GAME_CHECKPOINTS_PREFIX, GAME_KEY_PREFIX } from "../constants/storage";

export interface ActiveGameMeta {
  id: string;
  mode: "ai" | "local" | "online";
  difficulty: string;
}

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
  } catch { /* best effort */ }
  const active = loadActiveGame();
  if (active?.id === gameId) {
    clearActiveGame();
  }
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
