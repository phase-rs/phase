import { describe, expect, it } from "vitest";

import type { GameAction, GameObject } from "../../adapter/types.ts";
import { buildGameObject } from "../../test/factories/gameObjectFactory.ts";
import { abilityChoiceLabel } from "../costLabel.ts";

/**
 * Relic of Legends' two activated mana abilities, copied VERBATIM from the real
 * 4-player playtest dump where the bug was observed
 * (`.kilo-dump/game-state-turn-1-2026-07-22T20-04-12-617Z.json`,
 * `gameState.objects["402"].abilities`) — the engine's own serialized surface,
 * not a hand-authored approximation.
 *
 * Both abilities produce the identical `AnyOneColor × 1`; the ONLY discriminator
 * the engine ships is the cost (`Tap` vs `TapCreatures{legendary creature you
 * control}`) and the per-ability `description`.
 */
const RELIC_OF_LEGENDS_ABILITIES = [
  {
    kind: "Activated",
    effect: {
      type: "Mana",
      produced: {
        type: "AnyOneColor",
        count: { type: "Fixed", value: 1 },
        color_options: ["White", "Blue", "Black", "Red", "Green"],
      },
    },
    cost: { type: "Tap" },
    sub_ability: null,
    duration: null,
    description: "{T}: Add one mana of any color.",
    target_prompt: null,
    condition: null,
    optional_targeting: false,
    optional: false,
    forward_result: false,
    is_mana_ability: true,
  },
  {
    kind: "Activated",
    effect: {
      type: "Mana",
      produced: {
        type: "AnyOneColor",
        count: { type: "Fixed", value: 1 },
        color_options: ["White", "Blue", "Black", "Red", "Green"],
      },
    },
    cost: {
      type: "TapCreatures",
      requirement: { requirement: "count", count: 1 },
      filter: {
        type: "Typed",
        type_filters: ["Creature"],
        controller: "You",
        properties: [{ type: "HasSupertype", value: "Legendary" }],
      },
    },
    sub_ability: null,
    duration: null,
    description: "Tap an untapped legendary creature you control: Add one mana of any color.",
    target_prompt: null,
    condition: null,
    optional_targeting: false,
    optional: false,
    forward_result: false,
    is_mana_ability: true,
  },
] as unknown as GameObject["abilities"];

function relicOfLegends(): GameObject {
  return buildGameObject({
    id: 402,
    card_id: 402,
    name: "Relic of Legends",
    zone: "Battlefield",
    card_types: { supertypes: [], core_types: ["Artifact"], subtypes: [] },
    back_face: null,
    abilities: RELIC_OF_LEGENDS_ABILITIES,
  });
}

function activate(index: number): GameAction {
  return { type: "ActivateAbility", data: { source_id: 402, ability_index: index } };
}

/** The rendered content of one ability-choice modal option (GamePage's ChoiceModal
 *  renders `label` plus the secondary `description`). Two options are
 *  distinguishable to the player iff this pair differs. */
function renderedOption(object: GameObject, index: number): string {
  const { label, description } = abilityChoiceLabel(activate(index), object);
  return `${label}\u0000${description ?? ""}`;
}

describe("mana-ability activation options must be distinguishable (Relic of Legends)", () => {
  it("gives each of a permanent's mana abilities its own rendered option", () => {
    const object = relicOfLegends();

    // Discriminating: the two options must NOT render identically, or the player
    // cannot tell "tap Relic itself" from "tap an untapped legendary creature".
    expect(renderedOption(object, 0)).not.toBe(renderedOption(object, 1));
  });

  it("surfaces each mana ability's own activation cost from the engine", () => {
    const object = relicOfLegends();
    const first = abilityChoiceLabel(activate(0), object);
    const second = abilityChoiceLabel(activate(1), object);

    // The engine is the only authority on what each cost is; the option must
    // carry that engine-provided text, not just the (identical) produced mana.
    const firstText = `${first.label} ${first.description ?? ""}`;
    const secondText = `${second.label} ${second.description ?? ""}`;
    expect(firstText).toContain("{T}");
    expect(secondText).toContain("Tap an untapped legendary creature you control");
  });

  it("still tells the player what each ability produces", () => {
    const object = relicOfLegends();
    for (const index of [0, 1]) {
      const { label, description } = abilityChoiceLabel(activate(index), object);
      expect(`${label} ${description ?? ""}`).toContain("one mana of any color");
    }
  });
});
