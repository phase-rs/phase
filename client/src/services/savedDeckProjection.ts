import type { GameFormat } from "../adapter/types";
import type { CommanderBracket } from "../types/bracket";
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

/**
 * Persisted marker: this record's commander-family companion is not also listed
 * in its sideboard (Maybeboard). Records written before the dedicated slot
 * existed double-listed it; the projection below removes that copy once and
 * sets the marker, so a later load or boot migration never removes a second,
 * legitimate copy (a two-copy Commander Draft pool).
 */
const COMPANION_NETTED = "companion_netted";

/** The persisted JSON of a saved deck. Every writer stores the netted shape. */
export function serializeSavedDeck(deck: ParsedDeck, format: GameFormat, bracket: CommanderBracket | null): string {
  return JSON.stringify({
    ...projectSignatureSpellForFormat(deck, format),
    format,
    ...(bracket !== null ? { bracket } : {}),
    ...(deck.companion ? { [COMPANION_NETTED]: true } : {}),
  });
}

/**
 * The saved-deck form of a deck whose sideboard still holds the designated
 * companion's own copy (a draft pool remainder). A commander-family
 * designation is a card outside the deck that no sideboard carries
 * (CR 903.5e), so one copy leaves the sideboard; a Limited designation stays a
 * sideboard card (CR 100.4b) and keeps it.
 */
export function netCompanionFromPoolSideboard(deck: ParsedDeck, format: GameFormat): ParsedDeck {
  if (!deck.companion || formatMetadata(format)?.companion_slot.type !== "CommanderFamily") return deck;
  return { ...deck, sideboard: removeOneCopy(deck.sideboard, deck.companion) };
}

/** Keeps the Oathbreaker-only signature slot out of other persisted formats. */
export function projectSignatureSpellForFormat(
  deck: ParsedDeck,
  format: GameFormat | undefined,
): ParsedDeck {
  return format === "Oathbreaker" ? deck : { ...deck, signature_spell: undefined };
}

/**
 * Projects persisted companion and signature-spell slots for their format, by
 * the engine-published companion slot (`FormatMetadata.companion_slot`):
 *
 * - `CommanderFamily`: keep the dedicated companion; a record without the
 *   netted marker drops its one stale Maybeboard copy, once.
 * - `LimitedPool`: keep the designation and its physical copy in the sideboard
 *   (CR 100.4b: the pool outside the main deck is the sideboard).
 * - `SideboardOnly`: the format has no slot, so the card stays only as a
 *   sideboard copy.
 */
export function projectSavedDeckSpecialSlots(
  saved: ParsedDeck & Record<string, unknown>,
  repaired: ParsedDeck,
): ParsedDeck {
  const format = typeof saved.format === "string" ? saved.format as GameFormat : undefined;
  const projected = projectSignatureSpellForFormat(repaired, format);
  const companion = saved.companion;
  if (!companion) return projected;

  const withSideboardCopy = projected.sideboard.some((entry) => entry.name === companion)
    ? projected.sideboard
    : [...projected.sideboard, { count: 1, name: companion }];
  const slot = format !== undefined ? formatMetadata(format)?.companion_slot.type : undefined;
  switch (slot) {
    case "CommanderFamily":
      return {
        ...projected,
        sideboard: saved[COMPANION_NETTED] === true
          ? projected.sideboard
          : removeOneCopy(projected.sideboard, companion),
        [COMPANION_NETTED]: true,
      } as ParsedDeck;
    case "LimitedPool":
      return { ...projected, sideboard: withSideboardCopy, companion };
    case "SideboardOnly":
    case undefined:
      return { ...projected, sideboard: withSideboardCopy, companion: undefined };
  }
}
