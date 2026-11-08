import { describe, expect, it } from "vitest";

import { withChallengeDeck } from "../challengeDeckRegistry";
import { formatMetadata } from "../formatRegistry";

/**
 * Unit coverage for `withChallengeDeck` that needs no WASM binary (the
 * `.integration.test.ts` sibling pins the mirror against the engine's
 * `getChallengeDeckRegistry` export in CI, where the binary exists).
 */
describe("withChallengeDeck", () => {
  it("applying a deck swaps the engine-resolved survivor shape", () => {
    const hordeConfig = formatMetadata("Horde")?.default_config;
    expect(hordeConfig).toBeDefined();

    // Sauron survivors are Commander: 100-card singleton decks with a
    // commander and a 21-damage threshold — the engine-resolved shape for
    // the picked deck, not the previous deck's leftovers.
    const edh = withChallengeDeck(hordeConfig!, "SauronHorde");
    expect(edh.deck_size).toEqual({ type: "Exactly", data: 100 });
    expect(edh.singleton).toBe(true);
    expect(edh.uses_commander).toBe(true);
    expect(edh.commander_damage_threshold).toBe(21);
    expect(edh.default_deck_copy_limit).toEqual({ type: "UpTo", data: 1 });

    // Switching to a constructed deck (D&D) restores the 60-card
    // non-singleton shape — nothing of the EDH axes may linger.
    const constructed = withChallengeDeck(edh, "DndHorde");
    expect(constructed.deck_size).toEqual({ type: "Minimum", data: 60 });
    expect(constructed.singleton).toBe(false);
    expect(constructed.uses_commander).toBe(false);
    expect(constructed.commander_damage_threshold).toBeNull();
    expect(constructed.default_deck_copy_limit).toEqual({
      type: "Unlimited",
    });

    // Setup-screen edits and the transport remap survive the switch.
    const edited = {
      ...hordeConfig!,
      starting_life: 40,
      archenemy_player: 1,
    };
    const switched = withChallengeDeck(edited, "SauronHorde");
    expect(switched.starting_life).toBe(40);
    expect(switched.archenemy_player).toBe(1);
    expect(switched.min_players).toBe(hordeConfig!.min_players);
    expect(switched.max_players).toBe(hordeConfig!.max_players);
  });

  it("leaves non-Horde formats untouched", () => {
    const std = formatMetadata("Standard")?.default_config;
    expect(std).toBeDefined();
    expect(withChallengeDeck(std!, "SauronHorde")).toBe(std);
  });

  it("carries the engine-authored haste axis with the picked deck", () => {
    // Selecting Turtle must preserve the engine's `false` (booklet p. 6:
    // newly entered creatures can't attack that turn) — an absent field
    // would deserialize engine-side as `true` via the pre-axis fallback.
    const hordeConfig = formatMetadata("Horde")?.default_config;
    expect(hordeConfig).toBeDefined();

    const turtle = withChallengeDeck(hordeConfig!, "TurtleTeamUpHorde");
    expect(turtle.horde_ruleset?.horde_creatures_haste).toBe(false);

    // Every classic mill-race Horde grants haste.
    for (const deck of [
      "CybermanHorde",
      "DndHorde",
      "ZombiesHorde",
      "SliversHorde",
      "HumansGodzillaHorde",
      "SauronHorde",
      "SarumanHorde",
      "LotrTwoTowersHorde",
    ] as const) {
      expect(withChallengeDeck(hordeConfig!, deck).horde_ruleset?.horde_creatures_haste).toBe(
        true,
      );
    }

    // Switching away from Turtle must not leave its `false` attached.
    const backToClassic = withChallengeDeck(turtle, "DndHorde");
    expect(backToClassic.horde_ruleset?.horde_creatures_haste).toBe(true);
  });
});
