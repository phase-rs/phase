import type { CommanderBracket } from "../types/bracket";

/**
 * Hand-curated bracket tier for bundled Commander precons, keyed by the
 * precon deckId (the MTGJSON filename stem used in `client/public/decks.json`,
 * e.g. `AdaptiveEnchantment_C18`).
 *
 * This overlay is the source of truth until the Rust precon-export pipeline
 * is taught to emit `bracket` directly. Entries are intentionally additive —
 * a precon with no entry surfaces as `null` (Unrated), which matches how
 * untagged user decks behave in the AI random pool.
 *
 * **Curation policy:** assign conservatively. When in doubt, prefer the
 * lower tier — the filter is opt-in and overshooting bracket 4 or 5 will
 * mismatch the user's expectations more than undershooting bracket 2 or 3.
 */
export const PRECON_BRACKETS: Readonly<Record<string, CommanderBracket>> = {
  // Curators: add entries here as you tag bundled precons. Examples:
  // "AdaptiveEnchantment_C18": 2,
  // "ArcaneMaelstrom_C20": 2,
};

export function getPreconBracket(deckId: string): CommanderBracket | null {
  return PRECON_BRACKETS[deckId] ?? null;
}
