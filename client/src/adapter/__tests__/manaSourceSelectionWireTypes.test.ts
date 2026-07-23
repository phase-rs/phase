import { describe, expect, it } from "vitest";

import type { GameAction, ManaRestriction } from "../types";
import {
  RESTRICTION_LABEL_KEYS,
  restrictionTag,
} from "../../viewmodel/manaPoolGroups";

describe("mana-source selection wire types", () => {
  it("round-trips every engine mana-restriction wire shape", () => {
    const restrictions: ManaRestriction[] = [
      "OnlyForSpell",
      { OnlyForSpellType: "Artifact" },
      { OnlyForCreatureType: "Elf" },
      {
        OnlyForTypeSpellsOrAbilities: {
          spell_type: "Creature",
          ability: "OfSpellType",
        },
      },
      "OnlyForActivation",
      { OnlyForTaggedActivation: { type: "Equip" } },
      "OnlyForXCosts",
      { OnlyForSpellWithKeywordKind: "Flashback" },
      { OnlyForSpellWithKeywordKindFromZone: ["Flashback", "Graveyard"] },
      { OnlyForSpellWithManaValue: { comparator: "GE", value: 4 } },
      {
        OnlyForSpellMatchingCostCriteria: {
          spell_type: "Creature",
          criteria: [
            { ManaValue: { comparator: "GE", value: 4 } },
            "HasXInCost",
          ],
        },
      },
      { OnlyForSpellWithColorCount: { comparator: "EQ", count: 2 } },
      {
        OnlyForSpellFromZone: {
          zone: "Hand",
          polarity: "NotFrom",
        },
      },
      "OnlyForFaceDownSpell",
      {
        OnlyForAny: [
          { OnlyForSpellType: "Artifact" },
          "OnlyForActivation",
        ],
      },
      { OnlyForSpecialAction: "UnlockDoor" },
      "ConvokePayment",
    ];
    const action: GameAction = {
      type: "TapLandForMana",
      data: {
        selection: {
          source: { object_id: 17, incarnation: 3 },
          ability_index: 0,
          mana_type: "Blue",
          atomic_combination: null,
          restrictions,
          penalty: "None",
          taps_for_mana: [],
        },
      },
    };

    expect(JSON.parse(JSON.stringify(action))).toEqual(action);
    for (const restriction of restrictions) {
      expect(RESTRICTION_LABEL_KEYS[restrictionTag(restriction)]).toMatch(/^manaPool\./);
    }
  });
});
