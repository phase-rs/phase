import { createStore, get, set } from "idb-keyval";

import type { GameFormat } from "../adapter/types";
import { MATCH_HISTORY_KEY, MATCH_HISTORY_STORE } from "../constants/storage";

// ── Data model ────────────────────────────────────────────────────────────────

export type MatchOutcome = "win" | "loss" | "draw";

export type MatchMode =
  | "ai"
  | "local"
  | "online"
  | "p2p-host"
  | "p2p-join"
  | "draft-match";

/** MTG mana color abbreviation. */
export type ManaColor = "W" | "U" | "B" | "R" | "G";

/**
 * Compact record of a completed game. Stored in IndexedDB as an ordered array;
 * never includes the full GameState (too large). Only the fields needed to
 * render statistics and the history list are captured.
 */
export interface MatchRecord {
  /** Stable ID — crypto.randomUUID() at recording time. */
  id: string;
  /** Unix epoch ms at game start. */
  startedAt: number;
  /** Unix epoch ms at game end. */
  endedAt: number;
  /** MTG format played. */
  format: GameFormat | string;
  /** How the game was played. */
  mode: MatchMode;
  /** Result from the local player's perspective. */
  outcome: MatchOutcome;
  /** Final turn number at game end. */
  turnCount: number;
  /** Local player's final life total. */
  playerLife: number;
  /** Opponent's final life total (first non-local player for multiplayer). */
  opponentLife: number;
  /** Number of players in the game. */
  playerCount: number;
  /** Name of the deck the local player used, if known. */
  deckName: string | null;
  /** Mana color identity of the deck (e.g. ["W","U"] for Azorius). */
  deckColors: ManaColor[];
  /** AI difficulty string, present for ai-mode games. */
  aiDifficulty?: string;
  /** Commander card name for Commander-family formats, if known. */
  commanderName?: string | null;
  /** Whether the local player mulliganed at least once. */
  tookMulligan?: boolean;
  /** Number of mulligans taken (0 if none). */
  mulliganCount?: number;
}

// ── IDB store ─────────────────────────────────────────────────────────────────

let _store: ReturnType<typeof createStore> | undefined;
function getStore(): ReturnType<typeof createStore> {
  if (!_store) _store = createStore(MATCH_HISTORY_STORE, MATCH_HISTORY_STORE);
  return _store;
}

// ── Read ──────────────────────────────────────────────────────────────────────

/** Load all match records from IndexedDB, newest first. */
export async function loadMatchHistory(): Promise<MatchRecord[]> {
  try {
    const records = await get<MatchRecord[]>(MATCH_HISTORY_KEY, getStore());
    return records ?? [];
  } catch {
    return [];
  }
}

// ── Write ─────────────────────────────────────────────────────────────────────

/** Prepend a new record and persist. Returns the updated list. */
export async function saveMatchRecord(record: MatchRecord): Promise<MatchRecord[]> {
  try {
    const existing = await loadMatchHistory();
    // Keep newest first; cap at 1000 so IDB stays bounded.
    const updated = [record, ...existing].slice(0, 1000);
    await set(MATCH_HISTORY_KEY, updated, getStore());
    return updated;
  } catch (err) {
    console.warn("[saveMatchRecord] IDB write failed:", err);
    return [];
  }
}

/** Remove a single record by ID. Returns the updated list. */
export async function deleteMatchRecord(id: string): Promise<MatchRecord[]> {
  try {
    const existing = await loadMatchHistory();
    const updated = existing.filter((r) => r.id !== id);
    await set(MATCH_HISTORY_KEY, updated, getStore());
    return updated;
  } catch {
    return [];
  }
}

/** Wipe all history. */
export async function clearMatchHistory(): Promise<void> {
  try {
    await set(MATCH_HISTORY_KEY, [], getStore());
  } catch (err) {
    console.warn("[clearMatchHistory] IDB write failed:", err);
  }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/** Infer a deck's mana color identity from its color string array representation.
 *  Accepts strings like "WU", "B", "WUBRG", or individual color letters. */
export function parseColorIdentity(colors: string[]): ManaColor[] {
  const valid = new Set<ManaColor>(["W", "U", "B", "R", "G"]);
  const result = new Set<ManaColor>();
  for (const s of colors) {
    for (const ch of s.toUpperCase()) {
      if (valid.has(ch as ManaColor)) result.add(ch as ManaColor);
    }
  }
  return [...result];
}

/** Derive color identity from a deck's color_identity field (array of color
 *  strings like ["W","U"]) or from the identity string directly. */
export function colorsFromDeckData(
  colorIdentity: string[] | string | null | undefined,
): ManaColor[] {
  if (!colorIdentity) return [];
  const arr = Array.isArray(colorIdentity) ? colorIdentity : [colorIdentity];
  return parseColorIdentity(arr);
}
