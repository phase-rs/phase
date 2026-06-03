import type { DeckEntry } from "../services/deckParser";

/** Fisher–Yates shuffle for deck-list preview (display only, not game logic). */
function shuffleNames(names: string[]): string[] {
  const copy = [...names];
  for (let i = copy.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [copy[i], copy[j]] = [copy[j], copy[i]];
  }
  return copy;
}

export function expandDeckNames(entries: DeckEntry[]): string[] {
  const names: string[] = [];
  for (const entry of entries) {
    for (let i = 0; i < entry.count; i++) {
      names.push(entry.name);
    }
  }
  return names;
}

export function sampleOpeningHand(entries: DeckEntry[], size = 7): string[] {
  const names = expandDeckNames(entries);
  if (names.length <= size) return names;
  return shuffleNames(names).slice(0, size);
}
