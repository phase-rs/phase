import { DraftAdapter } from "../adapter/draft-adapter";

// ── Limited companion designation backing (CR 702.139a + CR 100.4b) ─────
//
// The ENGINE is the single authority on whether a designated companion is a
// card the player owns outside the game: draft-core's
// `companion_has_drafted_copy_outside_deck` is the rule `validate_limited_deck`
// enforces at submission. This module only carries the delegation call — the
// display never counts copies itself.

/**
 * Ask the engine whether `companion` is backed by a drafted pool copy that the
 * main deck does not use. Names only: the pool's card names and the main
 * deck's card names, each one entry per copy.
 */
export function companionHasDraftedCopyOutsideDeck(
  poolNames: readonly string[],
  mainDeckNames: readonly string[],
  companion: string,
): Promise<boolean> {
  return new DraftAdapter().companionHasDraftedCopyOutsideDeck(
    poolNames,
    mainDeckNames,
    companion,
  );
}
