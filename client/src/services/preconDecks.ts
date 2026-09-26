import {
  clearDeckAutosaveMarker,
  savedDeckUnchanged,
  STORAGE_KEY_PREFIX,
  writeSavedDeckData,
  type SavedDeckSnapshot,
} from "../constants/storage";
import type { DeckEntry } from "../hooks/useDecks";
import type { ParsedDeck } from "./deckParser";
import { withSavedDeckLibrary } from "./savedDeckTransaction";

export function preconDeckEntryToParsedDeck(deck: DeckEntry): ParsedDeck {
  return {
    main: deck.mainBoard.map((c) => ({ name: c.name, count: c.count })),
    sideboard: (deck.sideBoard ?? []).map((c) => ({ name: c.name, count: c.count })),
    commander:
      deck.commander && deck.commander.length > 0
        ? deck.commander.map((c) => c.name)
        : undefined,
  };
}

export function preconExists(savedName: string): boolean {
  return localStorage.getItem(STORAGE_KEY_PREFIX + savedName) !== null;
}

/**
 * Whether a name that already exists in the library should be replaced or left as-is.
 * `"replace"` with `expected` given only replaces if the name still holds the deck `expected`
 * captured at the moment the caller confirmed the overwrite; without it, `"replace"` is
 * unconditional (there was nothing to confirm overwriting).
 */
export type ExistingDeckPolicy = { type: "keep" } | { type: "replace"; expected?: SavedDeckSnapshot };

/**
 * Persist a preconstructed deck under the user's saved-decks namespace so it
 * participates in the normal deck-compatibility / active-deck / tile-render
 * flows without any precon-specific branching downstream. When `onExisting` is
 * `{ type: "keep" }`, a deck already saved under `savedName` is left untouched;
 * with `{ type: "replace", expected }`, it is left untouched (resolving
 * `"kept-existing"`) if it no longer holds the deck `expected` captured.
 */
export function savePreconDeck(
  savedName: string,
  deck: DeckEntry,
  onExisting: ExistingDeckPolicy,
): Promise<"saved" | "kept-existing"> {
  const parsed = preconDeckEntryToParsedDeck(deck);
  return withSavedDeckLibrary((txn) => {
    if (onExisting.type === "keep") {
      if (localStorage.getItem(STORAGE_KEY_PREFIX + savedName) !== null) return "kept-existing";
    } else if (onExisting.expected && !savedDeckUnchanged(txn, onExisting.expected)) {
      return "kept-existing";
    }
    writeSavedDeckData(txn, savedName, JSON.stringify(parsed));
    clearDeckAutosaveMarker(txn, savedName);
    return "saved";
  });
}
