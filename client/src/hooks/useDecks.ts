import { useEffect, useState } from "react";

export interface DeckCardEntry {
  name: string;
  count: number;
}

export interface DeckEntry {
  code: string;
  name: string;
  type: string;
  releaseDate?: string;
  /** Share of mainboard+commander copies (counting duplicates) the engine
   *  can play right now, rounded to the nearest percent. 100 = fully
   *  playable; lower values mean some cards will silently no-op. */
  coveragePct: number;
  /** Unique card names the engine can't play yet. Empty/omitted at 100%. */
  unsupported?: string[];
  mainBoard: DeckCardEntry[];
  sideBoard?: DeckCardEntry[];
  commander?: DeckCardEntry[];
}

export type DeckMap = Record<string, DeckEntry>;

export function isCommanderPreconDeck(deck: DeckEntry): boolean {
  return deck.type === "Commander Deck";
}

let cached: DeckMap | null = null;
let fetchPromise: Promise<DeckMap | null> | null = null;

export function loadPreconDeckMap(): Promise<DeckMap | null> {
  if (!fetchPromise) {
    fetchPromise = fetch(__DECKS_URL__)
      .then((res) => (res.ok ? (res.json() as Promise<DeckMap>) : null))
      .then((data) => {
        if (data && typeof data === "object") cached = data;
        return cached;
      })
      .catch(() => null);
  }
  return fetchPromise;
}

export type UseDecksStatus = "loading" | "success" | "error";

export interface UseDecksResult {
  /** Catalog entries keyed by MTGJSON filename stem; `null` until the first fetch settles. */
  decks: DeckMap | null;
  status: UseDecksStatus;
}

/**
 * Returns the preconstructed deck catalog keyed by deck id (MTGJSON filename
 * stem, e.g. `RedDeckB_10E`). Includes every deck above MIN_DECK_CARDS — each
 * entry carries a `coveragePct`, so consumers (e.g. the precon picker) can
 * apply their own coverage-floor filter rather than dropping decks at build
 * time.
 */
export function useDecks(): UseDecksResult {
  const [decks, setDecks] = useState<DeckMap | null>(cached);
  const [status, setStatus] = useState<UseDecksStatus>(cached ? "success" : "loading");

  useEffect(() => {
    if (cached) return;
    loadPreconDeckMap().then((d) => {
      if (d) {
        setDecks(d);
        setStatus("success");
        return;
      }
      setStatus("error");
    });
  }, []);

  return { decks, status };
}
