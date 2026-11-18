import { useMemo } from "react";

import type { ChallengeDeckMetadata } from "../adapter/types.ts";
import { challengeDeckMetadata } from "../data/challengeDeckRegistry.ts";
import { useGameStore } from "../stores/gameStore.ts";

/**
 * The active Horde challenge deck's display metadata (label + engine-authored
 * `rules` summary) for the current game, or `undefined` when this isn't a Horde
 * game.
 *
 * Reads the challenge-deck identity from the in-game format config
 * (`format_config.horde_ruleset.challenge_deck`) and looks it up in the same
 * engine-owned registry the setup deck picker uses. The engine owns the rules
 * wording — in-game Horde surfaces render `metadata.rules` verbatim and never
 * re-derive rules prose from the structured `HordeRuleset` on the client.
 */
export function useHordeDeckMeta(): ChallengeDeckMetadata | undefined {
  const deck = useGameStore(
    (s) => s.gameState?.format_config?.horde_ruleset?.challenge_deck,
  );
  return useMemo(() => (deck ? challengeDeckMetadata(deck) : undefined), [deck]);
}
