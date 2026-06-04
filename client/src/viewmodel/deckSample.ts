import type { DeckEntry } from "../services/deckParser";
import { expandEntries } from "../services/deckParser";

/** Fisher–Yates shuffle for deck-list preview (display only, not game logic). */
function shuffleNames(names: string[]): string[] {
  const copy = [...names];
  for (let i = copy.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [copy[i], copy[j]] = [copy[j], copy[i]];
  }
  return copy;
}

export function sampleOpeningHand(entries: DeckEntry[], size = 7): string[] {
  const names = expandEntries(entries);
  if (names.length <= size) return names;
  return shuffleNames(names).slice(0, size);
}
