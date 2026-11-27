import { describe, expect, it } from "vitest";

import i18n from "../../i18n";
import { manaRestrictionLabel, projectedManaChoices } from "../GamePage";
import type {
  InteractionActionId,
  InteractionChoice,
  InteractionChoiceId,
  InteractionId,
  InteractionPresentationSurface,
  ViewerInteraction,
} from "../../adapter/generated/interaction";
import type { ManaUnit, ObjectId } from "../../adapter/types";
import { groupManaPoolUnits, manaGroupTooltip } from "../../viewmodel/manaPoolGroups";

const FOREST: ObjectId = 42;
const MOUNTAIN: ObjectId = 43;

function tapChoice(actionId: string, source: ObjectId, symbol: string): InteractionChoice {
  const surfaces: InteractionPresentationSurface[] = [
    {
      type: "action",
      data: { code: "tapLandForMana", actionId: actionId as InteractionActionId },
    },
    {
      type: "object",
      data: {
        role: "source",
        index: null,
        reference: String(source),
        name: null,
        zone: "battlefield",
        controller: 0,
        power: null,
        tapped: false,
      },
    },
    {
      type: "mana",
      data: { role: "producedMana", index: null, symbols: [symbol], restrictions: [] },
    },
  ];
  return {
    id: `${actionId}-choice` as InteractionChoiceId,
    surfaces,
    status: { type: "available" },
  };
}

function interactionWith(choices: InteractionChoice[]): ViewerInteraction {
  return {
    waitingForKind: { simultaneous: null, terminal: false, code: "choose" },
    authorizedSubmitters: [0],
    canSubmit: true,
    autoPassRecommended: false,
    opportunities: [
      {
        interactionId: "session.0.1" as InteractionId,
        response: { type: "exactChoices", data: { choices } },
        surfaces: [],
        progress: {
          selected: 0,
          minimum: 1,
          maximum: 1,
          aggregate: null,
          confirmable: false,
        },
      },
    ],
    attachmentFans: {},
  attachmentViews: {},
    availability: { type: "inputRequired" },
  };
}

describe("projectedManaChoices", () => {
  it("labels a cast-from-zone prohibition without presenting it as spell-only mana", () => {
    expect(
      manaRestrictionLabel(i18n.getFixedT("en", "game"), {
        type: "cannotCastSpellFromZone",
        data: { zone: "hand" },
      }),
    ).toBe("Can't be spent to cast spells from hand.");
  });

  it("labels distinct spell restriction scopes in mana choices and the pool", () => {
    const t = i18n.getFixedT("en", "game");
    const spellType = "Artifact";
    const prohibition = "Can't be spent to cast spells outside the Artifact type.";
    for (const [ability, label] of [
      ["ofSpellType", "Only for Artifact spells or abilities of Artifacts."],
      ["any", "Only for Artifact spells or abilities."],
      ["anyNonSpellPayment", prohibition],
    ] as const) {
      expect(manaRestrictionLabel(t, {
        type: "onlyForTypeSpellsOrAbilities",
        data: { spellType, ability },
      })).toBe(label);
    }

    const units = (["Any", "AnyNonSpellPayment"] as const).map((ability, index): ManaUnit => ({
      color: "Blue",
      source_id: FOREST,
      pip_id: index + 1,
      snow: false,
      restrictions: [{ OnlyForTypeSpellsOrAbilities: { spell_type: spellType, ability } }],
    }));
    const groups = groupManaPoolUnits(units);
    expect(groups.map((group) => group.pipIds)).toEqual([[1], [2]]);
    expect(groups.map((group) => manaGroupTooltip((key, options) => t(key, options), group))).toEqual([
      "Spend only on spells or abilities of a specific type",
      prohibition,
    ]);
  });

  it("keys each tapLandForMana choice by its action id", () => {
    const choices = projectedManaChoices(
      interactionWith([tapChoice("act-green", FOREST, "G")]),
      FOREST,
    );

    expect([...choices.keys()]).toEqual(["act-green"]);
    expect(choices.get("act-green")).toContainEqual({
      type: "mana",
      data: { role: "producedMana", index: null, symbols: ["G"], restrictions: [] },
    });
  });

  it("returns only the choices whose source surface is the requested object", () => {
    const interaction = interactionWith([
      tapChoice("act-green", FOREST, "G"),
      tapChoice("act-red", MOUNTAIN, "R"),
    ]);

    expect([...projectedManaChoices(interaction, FOREST).keys()]).toEqual(["act-green"]);
    expect([...projectedManaChoices(interaction, MOUNTAIN).keys()]).toEqual(["act-red"]);
  });

  // An engine whose interaction authority is unbound reports
  // `availability: unsupported { authorityUnbound }` and — critically — an empty
  // `opportunities` list, which is what the mana choice modal received for as
  // long as no production code path called `bind_interaction_authority`. This
  // pins the resulting behaviour so a future regression is visible as a silently
  // unlabelled modal rather than an error.
  it("yields nothing when the engine reports no opportunities", () => {
    const unbound: ViewerInteraction = {
      ...interactionWith([]),
      opportunities: [],
      availability: { type: "unsupported", data: { reason: "authorityUnbound" } },
    };

    expect(projectedManaChoices(unbound, FOREST).size).toBe(0);
    expect(projectedManaChoices(null, FOREST).size).toBe(0);
  });
});
