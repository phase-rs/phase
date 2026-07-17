import type { GameFormat } from "../adapter/types";
import { formatMetadata } from "../data/formatRegistry";
import type { ParsedDeck } from "./deckParser";

function removeOneCopy(entries: ParsedDeck["sideboard"], name: string): ParsedDeck["sideboard"] {
  let removed = false;
  return entries.flatMap((entry) => {
    if (removed || entry.name !== name) return [entry];
    removed = true;
    return entry.count > 1 ? [{ ...entry, count: entry.count - 1 }] : [];
  });
}

/** Keeps the Oathbreaker-only signature slot out of other persisted formats. */
export function projectSignatureSpellForFormat(
  deck: ParsedDeck,
  format: GameFormat | undefined,
): ParsedDeck {
  return format === "Oathbreaker" ? deck : { ...deck, signature_spell: undefined };
}

/**
 * Projects persisted companion and signature-spell slots for their format.
 *
 * Commander-family decks retain their dedicated companion and remove exactly
 * one stale Maybeboard copy. Traditional formats instead retain one sideboard
 * copy and clear the dedicated Commander-family slot.
 */
export function projectSavedDeckSpecialSlots(
  saved: ParsedDeck & Record<string, unknown>,
  repaired: ParsedDeck,
): ParsedDeck {
  const format = typeof saved.format === "string" ? saved.format as GameFormat : undefined;
  const projected = projectSignatureSpellForFormat(repaired, format);
  if (!saved.companion) return projected;

  const usesCommander = format !== undefined
    && formatMetadata(format)?.default_config.uses_commander === true;
  if (usesCommander) {
    return {
      ...projected,
      sideboard: removeOneCopy(projected.sideboard, saved.companion),
    };
  }

  return {
    ...projected,
    sideboard: projected.sideboard.some((entry) => entry.name === saved.companion)
      ? projected.sideboard
      : [...projected.sideboard, { count: 1, name: saved.companion }],
    companion: undefined,
  };
}
