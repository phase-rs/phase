import { describe, expect, it } from "vitest";

import type { AdditionalCost, GameAction, GameObject, Keyword, ManaCost } from "../../adapter/types.ts";
import { buildGameObject } from "../../test/factories/gameObjectFactory.ts";
import {
  abilityChoiceLabel,
  abilityLabel,
  additionalCostChoices,
  formatAbilityCost,
  formatCost,
  spellCostDisplay,
  stripLoyaltyCostPrefix,
} from "../costLabel.ts";

function makeObject(overrides: Partial<GameObject> = {}): GameObject {
  return buildGameObject({
    id: 1,
    card_id: 100,
    name: "Test Card",
    card_types: { supertypes: [], core_types: [], subtypes: [] },
    back_face: null,
    ...overrides,
  });
}

describe("abilityChoiceLabel per-variant formatting", () => {
  it("labels CrewVehicle with the keyword N extracted from engine keywords", () => {
    const object = makeObject({
      name: "Skysovereign, Consul Flagship",
      keywords: [
        {
          Crew: { power: 3, once_per_turn: { type: "Unlimited" } },
        } as unknown as Keyword,
      ],
    });
    const action: GameAction = {
      type: "CrewVehicle",
      data: { vehicle_id: 1, creature_ids: [] },
    };
    const result = abilityChoiceLabel(action, object);
    expect(result.label).toBe("Crew 3");
    expect(result.description).toContain("total power 3 or greater");
  });

  it("falls back to 'Crew' when no Crew keyword is present (defensive)", () => {
    // Should never happen in practice, but guards against malformed data.
    const object = makeObject({ name: "Phantom Vehicle", keywords: [] });
    const action: GameAction = {
      type: "CrewVehicle",
      data: { vehicle_id: 1, creature_ids: [] },
    };
    expect(abilityChoiceLabel(action, object).label).toBe("Crew");
  });

  it("labels SaddleMount with Saddle N extracted from keywords", () => {
    const object = makeObject({
      name: "Rodeo Pyrohelix",
      keywords: [{ Saddle: 2 } as Keyword],
    });
    const action: GameAction = {
      type: "SaddleMount",
      data: { mount_id: 1, creature_ids: [] },
    };
    const result = abilityChoiceLabel(action, object);
    expect(result.label).toBe("Saddle 2");
    expect(result.description).toContain("total power 2 or greater");
  });

  it("labels ActivateStation with a fixed label and rules-text description", () => {
    const object = makeObject({
      name: "Monoist Gravliner",
      keywords: ["Station" as Keyword],
    });
    const action: GameAction = {
      type: "ActivateStation",
      data: { spacecraft_id: 1, creature_id: null },
    };
    const result = abilityChoiceLabel(action, object);
    expect(result.label).toBe("Station");
    expect(result.description).toContain("charge counters equal to its power");
  });

  it("labels Equip with a fixed label and rules-text description", () => {
    const object = makeObject({ name: "Sword of Feast and Famine" });
    const action: GameAction = {
      type: "Equip",
      data: { equipment_id: 1, target_id: 5 },
    };
    const result = abilityChoiceLabel(action, object);
    expect(result.label).toBe("Equip");
    expect(result.description).toContain("target creature you control");
  });

  it("labels ReturnToHand costs from ability description (Quirion Ranger)", () => {
    const ability = {
      cost: { type: "ReturnToHand", count: 1 },
      description:
        "Return a Forest you control to its owner's hand: Untap target creature.",
      effect: { type: "Untap" },
    } satisfies GameObject["abilities"][number];
    expect(abilityLabel(ability)).toBe(
      "Return a Forest you control to its owner's hand",
    );
    expect(formatCost({ type: "ReturnToHand", count: 1 })).toBe("Return 1 permanent");
  });

  it("labels an ActivateAbility with its serialized cost", () => {
    const object = makeObject({
      name: "Llanowar Elves",
      abilities: [
        {
          cost: { type: "Tap" },
          description: "{T}: Add {G}.",
          effect: {
            type: "Mana",
            produced: { type: "Fixed", colors: ["Green"] },
          },
        } satisfies GameObject["abilities"][number],
      ],
    });
    const action: GameAction = {
      type: "ActivateAbility",
      data: { source_id: 1, ability_index: 0 },
    };
    const result = abilityChoiceLabel(action, object);
    // Mana abilities surface the produced symbol, not the tap cost.
    expect(result.label).toBe("Add {G}");
  });

  it("labels an ActivateAbility that adds one mana of any color", () => {
    const object = makeObject({
      name: "Holdout Settlement",
      abilities: [
        {
          cost: {
            type: "Composite",
            costs: [
              { type: "Tap" },
              { type: "TapCreatures", count: 1 },
            ],
          },
          description: "{T}, Tap an untapped creature you control: Add one mana of any color.",
          effect: {
            type: "Mana",
            produced: {
              type: "AnyOneColor",
              count: { type: "Fixed", value: 1 },
              color_options: ["White", "Blue", "Black", "Red", "Green"],
            },
          },
        } satisfies GameObject["abilities"][number],
      ],
    });
    const action: GameAction = {
      type: "ActivateAbility",
      data: { source_id: 1, ability_index: 0 },
    };

    expect(abilityChoiceLabel(action, object).label).toBe("Add one mana of any color");
  });

  it("labels an ActivateAbility that adds multiple mana of any one color", () => {
    const object = makeObject({
      name: "Gilded Lotus",
      abilities: [
        {
          cost: { type: "Tap" },
          description: "{T}: Add three mana of any one color.",
          effect: {
            type: "Mana",
            produced: {
              type: "AnyOneColor",
              count: { type: "Fixed", value: 3 },
              color_options: ["White", "Blue", "Black", "Red", "Green"],
            },
          },
        } satisfies GameObject["abilities"][number],
      ],
    });
    const action: GameAction = {
      type: "ActivateAbility",
      data: { source_id: 1, ability_index: 0 },
    };

    expect(abilityChoiceLabel(action, object).label).toBe("Add 3 mana of any one color");
  });

  it("labels a non-mana ActivateAbility with its formatted cost", () => {
    const object = makeObject({
      name: "Quicksilver Dagger",
      abilities: [
        {
          cost: { type: "Tap" },
          description: "{T}: Draw a card.",
          effect: { type: "Draw" },
        } satisfies GameObject["abilities"][number],
      ],
    });
    const action: GameAction = {
      type: "ActivateAbility",
      data: { source_id: 1, ability_index: 0 },
    };
    const result = abilityChoiceLabel(action, object);
    expect(result.label).toBe("{T}");
    expect(result.description).toBe("Draw a card.");
  });

  it("attaches the engine's activation cost as a mana option's description (CR 602.1a)", () => {
    const object = makeObject({
      name: "Relic of Legends",
      abilities: [
        {
          cost: { type: "Tap" },
          description: "{T}: Add one mana of any color.",
          is_mana_ability: true,
          effect: {
            type: "Mana",
            produced: {
              type: "AnyOneColor",
              count: { type: "Fixed", value: 1 },
              color_options: ["White", "Blue", "Black", "Red", "Green"],
            },
          },
        } satisfies GameObject["abilities"][number],
      ],
    });
    const action: GameAction = { type: "ActivateAbility", data: { source_id: 1, ability_index: 0 } };
    const result = abilityChoiceLabel(action, object);

    expect(result.label).toBe("Add one mana of any color");
    // CR 602.1a: everything before the colon, taken from the engine's own description.
    expect(result.description).toBe("{T}");
  });

  it("does not attach a cost line to a loyalty ability that adds mana (CR 605.1a — Chandra, Torch of Defiance)", () => {
    const object = makeObject({
      name: "Chandra, Torch of Defiance",
      abilities: [
        {
          cost: { type: "Loyalty", amount: 1 },
          description: "+1: Add {R}{R}.",
          // Pinned so a fixture that silently MISSES the mana branch cannot pass: the
          // branch is what produces `label === "Add {R}{R}"`. A fixture falling to the
          // tail would get stripCostPrefix's bare-loyalty arm => "Add {R}{R}." (trailing
          // period) and the label assertion below would fail. That second assertion —
          // not any other test — is this test's reach-guard.
          effect: { type: "Mana", produced: { type: "Fixed", colors: ["Red", "Red"] } },
        } satisfies GameObject["abilities"][number],
      ],
    });
    const action: GameAction = { type: "ActivateAbility", data: { source_id: 1, ability_index: 0 } };
    const result = abilityChoiceLabel(action, object);

    expect(result.label).toBe("Add {R}{R}");          // reach-guard: proves the mana branch ran
    expect(result.description).toBeUndefined();       // CR 605.1a gate: no cost line

    // Mirrors pages/GamePage.tsx (`label: description ?? stripLoyaltyCostPrefix(label)`,
    // currently :2918, inside the badge branch opened at :2912). Replicated here because
    // mounting GamePage is prohibitively expensive; if GamePage's expression changes,
    // update this replica with it.
    expect(result.description ?? stripLoyaltyCostPrefix(result.label)).toBe("Add {R}{R}");
  });

  it("treats an absent is_mana_ability flag as not a mana ability", () => {
    const object = makeObject({
      name: "Unflagged Mana Source",
      abilities: [
        {
          // CR 605.1a: the engine's verdict is the flag. Absent must read as "not a mana
          // ability" (viewmodel/cardActionChoice.ts:29-32), never as "probably yes".
          cost: { type: "Tap" },
          description: "{T}: Add one mana of any color.",
          effect: {
            type: "Mana",
            produced: {
              type: "AnyOneColor",
              count: { type: "Fixed", value: 1 },
              color_options: ["White", "Blue", "Black", "Red", "Green"],
            },
          },
        } satisfies GameObject["abilities"][number],
      ],
    });
    const action: GameAction = { type: "ActivateAbility", data: { source_id: 1, ability_index: 0 } };
    const result = abilityChoiceLabel(action, object);

    expect(result.label).toBe("Add one mana of any color");  // reach-guard: the mana branch ran
    expect(result.description).toBeUndefined();
  });

  it('attaches no cost line to a mana ability the engine gave no description (no synthesized "Activate")', () => {
    const object = makeObject({
      name: "Descriptionless Mana Source",
      abilities: [
        {
          // `TapCreatures` has no `formatCost` arm, so without the `&& ability.description`
          // conjunct `abilityLabel` would fall through to `formatCost`'s
          // `default: "Activate"` (costLabel.ts:289-290) and that literal word would render
          // as this option's subtitle. Measured: dropping the conjunct yields "Activate".
          cost: {
            type: "TapCreatures",
            requirement: { type: "Count", count: 1 },
            filter: { type: "Any" },
          },
          is_mana_ability: true,
          effect: {
            type: "Mana",
            produced: {
              type: "AnyOneColor",
              count: { type: "Fixed", value: 1 },
              color_options: ["White", "Blue", "Black", "Red", "Green"],
            },
          },
        } satisfies GameObject["abilities"][number],
      ],
    });
    const action: GameAction = { type: "ActivateAbility", data: { source_id: 1, ability_index: 0 } };
    const result = abilityChoiceLabel(action, object);

    expect(result.label).toBe("Add one mana of any color");  // reach-guard: the mana branch ran
    expect(result.description).toBeUndefined();
    expect(result.description).not.toBe("Activate");
  });

  // CR 201.5: `~` binds to the object that has the ability. The NON-mana tail of
  // `abilityChoiceLabel` feeds the same ability-choice modal (GamePage.tsx:2899 →
  // ChoiceModal) as the mana branch above, so both must substitute or the modal shows one
  // substituted row next to a raw-tilde one. Both fixtures are verbatim engine descriptions
  // from the reported Kilo board dump
  // (`.kilo-dump/game-state-turn-1-2026-07-22T20-04-12-617Z.json`, objects 110 and 7).
  // Census population for every count in this test: that dump's 293 `kind: "Activated"`
  // abilities — 22 leak `~` into the cost text, 20 into the effect text. Widening to all 368
  // abilities (i.e. adding the 75 `kind: "Spell"` rows) gives 29/27 instead, so the two
  // number sets must not be mixed.
  it("substitutes ~ on the non-mana activated-ability path, in both label and description (CR 201.5)", () => {
    const action: GameAction = { type: "ActivateAbility", data: { source_id: 1, ability_index: 0 } };

    // Leak site 1 — the COST text, which becomes the option's `label`.
    const ghostQuarter = abilityChoiceLabel(
      action,
      makeObject({
        name: "Ghost Quarter",
        abilities: [
          {
            cost: {
              type: "Composite",
              costs: [{ type: "Tap" }, { type: "Sacrifice", count: 1 }],
            },
            description:
              "{T}, Sacrifice ~: Destroy target land. Its controller may search their library for a basic land card, put it onto the battlefield, then shuffle.",
            // Not `Mana`, so this fixture provably reaches the tail and not the branch at :415.
            effect: { type: "Destroy" },
          } satisfies GameObject["abilities"][number],
        ],
      }),
    );
    expect(ghostQuarter.label).toBe("{T}, Sacrifice Ghost Quarter");
    expect(ghostQuarter.description).toBe(
      "Destroy target land. Its controller may search their library for a basic land card, put it onto the battlefield, then shuffle.",
    );

    // Leak site 2 — the EFFECT text, which becomes the option's `description` after
    // `stripCostPrefix`. Disjoint from site 1 within that same `kind: "Activated"`
    // population: none of the 293 leaks into both. (All 7 overlaps in the wider 29/27 figures
    // are `kind: "Spell"` rows whose descriptions carry no colon, so `abilityLabel` and
    // `stripCostPrefix` both fall through to the whole string.)
    const hawkeye = abilityChoiceLabel(
      action,
      makeObject({
        name: "Hawkeye, Avenging Archer",
        abilities: [
          {
            cost: { type: "Tap" },
            description: "{T}: ~ deals 1 damage to any target.",
            effect: { type: "DealDamage" },
          } satisfies GameObject["abilities"][number],
        ],
      }),
    );
    expect(hawkeye.label).toBe("{T}");
    expect(hawkeye.description).toBe("Hawkeye, Avenging Archer deals 1 damage to any target.");
  });
});

describe("additionalCostChoices — multikicker (issue #454)", () => {
  const repeatableKicker: AdditionalCost = {
    type: "Kicker",
    data: {
      costs: [{ type: "Mana", cost: { type: "Cost", shards: [], generic: 2 } }],
      repeatable: true,
    },
  };

  it("first prompt (timesKicked 0) offers a non-cancel 'cast without kicking' decline", () => {
    const { title, options } = additionalCostChoices(repeatableKicker, 0);

    expect(title.toLowerCase()).toContain("multikicker");
    const pay = options.find((o) => o.id === "pay")!;
    const decline = options.find((o) => o.id === "decline")!;
    expect(pay.label).toContain("kick it");
    expect(decline.label).toBe("Cast without kicking");
    expect(decline.label.toLowerCase()).not.toContain("skip");
    expect(decline.label.toLowerCase()).not.toContain("cancel");
    expect(decline.description?.toLowerCase()).toContain("still resolves");
  });

  it("re-prompt (timesKicked 2) shows the kick count and a 'finish casting' decline", () => {
    const { title, options } = additionalCostChoices(repeatableKicker, 2);

    expect(title).toContain("kicked 2");
    const decline = options.find((o) => o.id === "decline")!;
    expect(decline.label).toContain("finish casting");
    expect(decline.label).toContain("(kicked 2×)");
    expect(decline.label.toLowerCase()).not.toContain("cancel");
  });
});

describe("additionalCostChoices — repeatable additional cost", () => {
  const repeatableCost: AdditionalCost = {
    type: "Optional",
    data: {
      cost: { type: "Mana", cost: { type: "Cost", shards: [], generic: 1 } },
      repeatable: true,
    },
  };

  it("first prompt offers a non-cancel decline", () => {
    const { title, options } = additionalCostChoices(repeatableCost, 0);

    expect(title).toContain("Pay additional cost");
    expect(options.find((o) => o.id === "pay")?.label).toBe("Pay {1}");
    expect(options.find((o) => o.id === "decline")?.label).toBe("Cast without paying");
  });

  it("re-prompt shows the payment count and finish-casting decline", () => {
    const { title, options } = additionalCostChoices(repeatableCost, 2);

    expect(title).toContain("paid 2");
    const decline = options.find((o) => o.id === "decline")!;
    expect(decline.label).toContain("finish casting");
    expect(decline.label).toContain("(paid 2×)");
  });
});

describe("formatAbilityCost", () => {
  it("formats disjunctive activation cost branches", () => {
    expect(formatAbilityCost({
      type: "OneOf",
      costs: [
        { type: "Mana", cost: { type: "Cost", shards: [], generic: 1 } },
        { type: "PayLife", amount: { type: "Fixed", value: 2 } },
      ],
    })).toBe("{1} or Pay 2 life");
  });
});

describe("spellCostDisplay", () => {
  const cost = (generic: number, shards: string[] = []): ManaCost => ({
    type: "Cost",
    generic,
    shards,
  });
  const noCost: ManaCost = { type: "NoCost" };

  it("shows the printed cost, not reduced, when the engine reports no override", () => {
    const printed = cost(7, ["Blue", "Blue", "Blue"]);
    const { displayCost, isReduced } = spellCostDisplay(undefined, printed);
    expect(displayCost).toBe(printed);
    expect(isReduced).toBe(false);
  });

  it("flags a smaller effective Cost as reduced", () => {
    const { displayCost, isReduced } = spellCostDisplay(cost(3, ["Blue"]), cost(5, ["Blue"]));
    expect(displayCost).toEqual(cost(3, ["Blue"]));
    expect(isReduced).toBe(true);
  });

  // CR 118.9: Omniscience reports the effective cost as NoCost against a real
  // printed cost — a reduction to {0}, so the pips must render (green {0}).
  it("flags a NoCost effective cost against a real printed cost as reduced (Omniscience)", () => {
    const { displayCost, isReduced } = spellCostDisplay(noCost, cost(7, ["Blue", "Blue", "Blue"]));
    expect(displayCost).toEqual(noCost);
    expect(isReduced).toBe(true);
  });

  // A naturally-free card (token, Ancestral Vision) has no printed cost and must
  // never be flagged reduced — no {0} overlay.
  it("does not flag a naturally-free card (no printed cost) as reduced", () => {
    const { isReduced } = spellCostDisplay(noCost, noCost);
    expect(isReduced).toBe(false);
  });

  it("does not flag an unchanged effective cost as reduced", () => {
    const { isReduced } = spellCostDisplay(cost(5, ["Blue"]), cost(5, ["Blue"]));
    expect(isReduced).toBe(false);
  });
});
