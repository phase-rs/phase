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

let cached: DeckMap | null = null;
let fetchPromise: Promise<DeckMap | null> | null = null;

function fetchDecks(): Promise<DeckMap | null> {
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

/**
 * Returns the preconstructed deck catalog keyed by deck id (MTGJSON filename
 * stem, e.g. `RedDeckB_10E`). Includes every deck above MIN_DECK_CARDS — each
 * entry carries a `coveragePct`, so consumers (e.g. the precon picker) can
 * apply their own coverage-floor filter rather than dropping decks at build
 * time. `null` while loading or on fetch failure.
 */
export function useDecks(): DeckMap | null {
  const [decks, setDecks] = useState<DeckMap | null>(cached);

  useEffect(() => {
    if (cached) return;
    fetchDecks().then((d) => { if (d) setDecks(d); });
  }, []);

  return decks;
}
