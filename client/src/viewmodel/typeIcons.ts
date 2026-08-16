import type { CoreType } from "../adapter/types.ts";

/**
 * Card core type → mana-font `ms-*` type glyph class. Every value is verified
 * present in `mana-font/css/mana.css` by the guardrail test. The `Record` is
 * keyed by the full `CoreType` union so a new engine core type is a compile
 * error here until it's mapped.
 */
export const CARD_TYPE_ICON_CLASS: Record<CoreType, string> = {
  Artifact: "ms-artifact",
  Creature: "ms-creature",
  Enchantment: "ms-enchantment",
  Instant: "ms-instant",
  Land: "ms-land",
  Planeswalker: "ms-planeswalker",
  Sorcery: "ms-sorcery",
  Battle: "ms-battle",
  Dungeon: "ms-dungeon",
  // No `ms-kindred` glyph exists; Tribal and Kindred are the old and new names
  // for the same card type, so both resolve to the tribal glyph.
  Tribal: "ms-tribal",
  Kindred: "ms-tribal",
};

/** Resolve a core type to its mana-font glyph class, or null when none ships. */
export function cardTypeIconClass(coreType: string): string | null {
  return CARD_TYPE_ICON_CLASS[coreType as CoreType] ?? null;
}
