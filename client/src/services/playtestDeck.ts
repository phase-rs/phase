import type { FormatConfig, GameFormat } from "../adapter/types";
import type { ParsedDeck } from "./deckParser";

export const PLAYTEST_DECK_SESSION_KEY = "phase:playtest-deck";

export interface PlaytestDeckPayload {
  deck: ParsedDeck;
  format: GameFormat;
  formatConfig?: FormatConfig;
}

export function stashPlaytestDeck(payload: PlaytestDeckPayload): void {
  sessionStorage.setItem(PLAYTEST_DECK_SESSION_KEY, JSON.stringify(payload));
}

export function consumePlaytestDeck(): PlaytestDeckPayload | null {
  const raw = sessionStorage.getItem(PLAYTEST_DECK_SESSION_KEY);
  if (!raw) return null;
  sessionStorage.removeItem(PLAYTEST_DECK_SESSION_KEY);
  try {
    return JSON.parse(raw) as PlaytestDeckPayload;
  } catch {
    return null;
  }
}
