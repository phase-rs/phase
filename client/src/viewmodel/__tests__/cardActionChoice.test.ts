import { describe, expect, it } from "vitest";

import type { GameAction, GameObject, WaitingFor } from "../../adapter/types.ts";
import type { ActivationAffordances, ObjectActivation } from "../cardActionChoice.ts";
import {
  collectObjectActions,
  deriveActivationAffordances,
  isManaObjectAction,
  requiresConfirmation,
  resolveDirectPlayOrCastAction,
  resolveObjectActivation,
  resolveSingleActionDispatch,
} from "../cardActionChoice.ts";
import { abilityChoiceLabel } from "../costLabel.ts";

function makeGameObject(overrides: Partial<GameObject> = {}): GameObject {
  return {
    id: 1,
    card_id: 100,
    owner: 0,
    controller: 0,
    zone: "Hand",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name: "Bala Ged Recovery",
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Sorcery"], subtypes: [] },
    mana_cost: { type: "Cost", shards: ["Green"], generic: 2 },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: ["Green"],
    base_power: null,
    base_toughness: null,
    base_keywords: [],
    base_color: ["Green"],
    timestamp: 1,
    entered_battlefield_turn: null,
    back_face: {
      name: "Bala Ged Sanctuary",
      power: null,
      toughness: null,
      card_types: { supertypes: [], core_types: ["Land"], subtypes: [] },
      mana_cost: { type: "NoCost" },
      keywords: [],
      abilities: [],
      color: [],
    },
    ...overrides,
  };
}

function tapLandAction(objectId: number): Extract<GameAction, { type: "TapLandForMana" }> {
  return {
    type: "TapLandForMana",
    data: {
      selection: {
        source: { object_id: objectId, incarnation: 1 },
        ability_index: null,
        mana_type: "Green",
        output: { type: "Concrete", data: "Green" },
        atomic_combination: null,
        restrictions: [],
        penalty: "None",
        taps_for_mana: [],
      },
    },
  };
}

describe("collectObjectActions", () => {
  it("returns the engine-provided bucket for the requested object", () => {
    // Engine-grouped map mirrors what `legal_actions_full` produces in Rust:
    // each key is a source ObjectId; each value is the subset of legal actions
    // whose `source_object()` equals that id. The viewmodel does not classify.
    const obj1Actions: GameAction[] = [
      { type: "PlayLand", data: { object_id: 1, card_id: 100 } },
      { type: "CastSpell", data: { object_id: 1, card_id: 100, targets: [] } },
      { type: "ActivateAbility", data: { source_id: 1, ability_index: 0 } },
      { type: "ActivateNinjutsu", data: { ninjutsu_object_id: 1, creature_to_return: 9 } },
      {
        type: "CastSpellAsWebSlinging",
        data: { hand_object: 1, card_id: 100, creature_to_return: 9 },
      },
    ];
    const obj2Actions: GameAction[] = [
      { type: "CastSpell", data: { object_id: 2, card_id: 200, targets: [] } },
    ];
    const grouped: Record<string, GameAction[]> = {
      "1": obj1Actions,
      "2": obj2Actions,
    };

    expect(collectObjectActions(grouped, 1)).toEqual(obj1Actions);
    expect(collectObjectActions(grouped, 2)).toEqual(obj2Actions);
    // Unknown id (e.g. a hand card with no legal actions): empty array, never undefined.
    expect(collectObjectActions(grouped, 999)).toEqual([]);
    // Missing map (e.g. pre-init): empty array, no crash.
    expect(collectObjectActions(undefined, 1)).toEqual([]);
  });
});

describe("isManaObjectAction", () => {
  it("recognizes only engine-provided mana actions", () => {
    const object = makeGameObject({
      abilities: [
        // CR 605.1a: the engine classifies mana abilities and exposes the
        // verdict as the derived `is_mana_ability` flag — isManaObjectAction
        // reads the flag rather than introspecting the effect AST.
        { is_mana_ability: true, effect: { type: "Mana" } },
        { effect: { type: "Draw" } },
      ],
    });

    expect(isManaObjectAction(tapLandAction(1), object)).toBe(true);
    expect(
      isManaObjectAction(
        { type: "TapForConvoke", data: { object_id: 1, mana_type: "Green" } },
        object,
      ),
    ).toBe(true);
    expect(
      isManaObjectAction(
        { type: "ActivateAbility", data: { source_id: 1, ability_index: 0 } },
        object,
      ),
    ).toBe(true);
    expect(
      isManaObjectAction(
        { type: "ActivateAbility", data: { source_id: 1, ability_index: 1 } },
        object,
      ),
    ).toBe(false);
    expect(
      isManaObjectAction(
        { type: "ActivateAbility", data: { source_id: 1, ability_index: 99 } },
        object,
      ),
    ).toBe(false);
    expect(
      isManaObjectAction(
        { type: "PlayLand", data: { object_id: 1, card_id: 100 } },
        object,
      ),
    ).toBe(false);
  });

  // Sprout Swarm regression: tapping a creature for convoke pays mana, so it
  // must classify as a mana action. Otherwise GameBoard never adds the
  // creature to `manaTappableObjectIds` during `WaitingFor::ManaPayment`,
  // and the click handler in PermanentCard has no path to dispatch the tap.
  it("treats TapForConvoke as a mana action so convoke creatures get the mana-tap ring", () => {
    const creature = makeGameObject({ card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Saproling"] } });
    expect(
      isManaObjectAction(
        { type: "TapForConvoke", data: { object_id: 1, mana_type: "Green" } },
        creature,
      ),
    ).toBe(true);
    expect(
      isManaObjectAction(
        { type: "TapForConvoke", data: { object_id: 1, mana_type: "Colorless" } },
        creature,
      ),
    ).toBe(true);
  });
});

describe("requiresConfirmation", () => {
  // #506: a lone card-consuming ActivateAbility (cycling) must NOT auto-fire.
  it("flags an ActivateAbility whose ability has consumes_source === true", () => {
    const object = makeGameObject({
      abilities: [{ effect: { type: "Draw" }, consumes_source: true }],
    });
    expect(
      requiresConfirmation(
        { type: "ActivateAbility", data: { source_id: 1, ability_index: 0 } },
        object,
      ),
    ).toBe(true);
  });

  // SHOULD-FIX 1: benign repeatable abilities ({T}: Scry 1) must not be gated.
  it("does not flag a benign ActivateAbility (consumes_source false/absent)", () => {
    const object = makeGameObject({
      abilities: [
        { effect: { type: "Scry" }, consumes_source: false },
        { effect: { type: "Scry" } },
      ],
    });
    expect(
      requiresConfirmation(
        { type: "ActivateAbility", data: { source_id: 1, ability_index: 0 } },
        object,
      ),
    ).toBe(false);
    expect(
      requiresConfirmation(
        { type: "ActivateAbility", data: { source_id: 1, ability_index: 1 } },
        object,
      ),
    ).toBe(false);
  });

  it("never flags PlayLand or CastSpell", () => {
    const object = makeGameObject();
    expect(
      requiresConfirmation({ type: "PlayLand", data: { object_id: 1, card_id: 100 } }, object),
    ).toBe(false);
    expect(
      requiresConfirmation(
        { type: "CastSpell", data: { object_id: 1, card_id: 100, targets: [] } },
        object,
      ),
    ).toBe(false);
  });

  it("flags CastPreparedCopy so the prepared spell is explicitly offered", () => {
    const object = makeGameObject();
    expect(
      requiresConfirmation({ type: "CastPreparedCopy", data: { source: 1 } }, object),
    ).toBe(true);
  });

  it("does not flag when the object is undefined", () => {
    expect(
      requiresConfirmation(
        { type: "ActivateAbility", data: { source_id: 1, ability_index: 0 } },
        undefined,
      ),
    ).toBe(false);
  });
});

describe("resolveSingleActionDispatch", () => {
  const cyclingAction: GameAction = {
    type: "ActivateAbility",
    data: { source_id: 1, ability_index: 0 },
  };
  const playLandAction: GameAction = {
    type: "PlayLand",
    data: { object_id: 1, card_id: 100 },
  };

  it("returns null for an empty action list", () => {
    expect(resolveSingleActionDispatch([], makeGameObject())).toBeNull();
  });

  it("returns null when more than one action is available", () => {
    expect(
      resolveSingleActionDispatch([playLandAction, cyclingAction], makeGameObject()),
    ).toBeNull();
  });

  it("auto-dispatches a lone PlayLand", () => {
    expect(resolveSingleActionDispatch([playLandAction], makeGameObject())).toBe(
      playLandAction,
    );
  });

  // #506 discriminating assertion — with the fix reverted this returns the
  // action instead of null and the card auto-cycles.
  it("returns null for a lone card-consuming ActivateAbility (cycling)", () => {
    const object = makeGameObject({
      abilities: [{ effect: { type: "Draw" }, consumes_source: true }],
    });
    expect(resolveSingleActionDispatch([cyclingAction], object)).toBeNull();
  });

  it("auto-dispatches a lone benign ActivateAbility", () => {
    const object = makeGameObject({
      abilities: [{ effect: { type: "Scry" }, consumes_source: false }],
    });
    expect(resolveSingleActionDispatch([cyclingAction], object)).toBe(cyclingAction);
  });

  it("returns null for a lone CastPreparedCopy", () => {
    const preparedAction: GameAction = { type: "CastPreparedCopy", data: { source: 1 } };
    expect(resolveSingleActionDispatch([preparedAction], makeGameObject())).toBeNull();
  });
});

describe("resolveDirectPlayOrCastAction", () => {
  const playLandAction: GameAction = {
    type: "PlayLand",
    data: { object_id: 1, card_id: 100 },
  };
  const cyclingAction: GameAction = {
    type: "ActivateAbility",
    data: { source_id: 1, ability_index: 0 },
  };

  it("returns the one unambiguous engine-provided play action", () => {
    expect(
      resolveDirectPlayOrCastAction({ "1": [playLandAction] }, makeGameObject()),
    ).toBe(playLandAction);
  });

  it("does not promise release-to-cast when another action requires a choice", () => {
    expect(
      resolveDirectPlayOrCastAction(
        { "1": [playLandAction, cyclingAction] },
        makeGameObject(),
      ),
    ).toBeNull();
  });

  it("does not classify a lone non-cast ability as release-to-cast", () => {
    expect(
      resolveDirectPlayOrCastAction(
        { "1": [cyclingAction] },
        makeGameObject({ abilities: [{ consumes_source: false }] }),
      ),
    ).toBeNull();
  });
});

describe("abilityChoiceLabel", () => {
  it("labels convoke tap actions by the mana they pay for", () => {
    const object = makeGameObject({
      name: "Venerated Loxodon",
    });

    expect(
      abilityChoiceLabel(
        { type: "TapForConvoke", data: { object_id: 1, mana_type: "Green" } },
        object,
      ),
    ).toEqual({
      label: "Tap for {G}",
      description: "Tap Venerated Loxodon to help pay this spell's cost.",
    });
    expect(
      abilityChoiceLabel(
        { type: "TapForConvoke", data: { object_id: 1, mana_type: "Colorless" } },
        object,
      ).label,
    ).toBe("Tap for {1}");
  });

  // CR 709.5e + CR 709.5b: the unlock special action pays the mana cost of a
  // LOCKED half, so the offer must name that half and show its cost. For a
  // permanent that is a COPY of a Room both come from the COPIED halves the
  // engine publishes — the recipient's own printed card has neither.
  //
  // Revert-failing assertion: both labels. Without an `UnlockRoomDoor` arm the
  // chain falls through to its `"Tap for Mana"` catch-all and the two offers
  // are indistinguishable.
  it("labels a Room door unlock with the half's name and its unlock cost", () => {
    const copy = makeGameObject({
      id: 7,
      name: "",
      zone: "Battlefield",
      card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Room"] },
      back_face: undefined,
    });
    const halves = {
      "7": {
        left: { name: "Greenhouse", mana_cost: { type: "Cost", shards: ["Green"], generic: 2 } },
        right: {
          name: "Rickety Gazebo",
          mana_cost: { type: "Cost", shards: ["Green"], generic: 3 },
        },
      },
    } as const;

    expect(
      abilityChoiceLabel(
        { type: "UnlockRoomDoor", data: { object_id: 7, door: "Left" } },
        copy,
        undefined,
        undefined,
        halves as never,
      ).label,
    ).toBe("Unlock Greenhouse ({2}{G})");

    expect(
      abilityChoiceLabel(
        { type: "UnlockRoomDoor", data: { object_id: 7, door: "Right" } },
        copy,
        undefined,
        undefined,
        halves as never,
      ).label,
    ).toBe("Unlock Rickety Gazebo ({3}{G})");
  });

  // CR 708.2a: a face-down permanent publishes no halves, so the engine sends
  // none and the label must stay honest rather than invent a name.
  it("falls back without inventing a name when no halves are published", () => {
    const copy = makeGameObject({ id: 7, name: "", zone: "Battlefield" });
    expect(
      abilityChoiceLabel(
        { type: "UnlockRoomDoor", data: { object_id: 7, door: "Left" } },
        copy,
      ).label,
    ).toBe("Unlock this door");
  });

  it("labels TapLandForMana with the engine-selected mana", () => {
    const object = makeGameObject({
      name: "Emergence Zone",
      card_types: {
        supertypes: [],
        core_types: ["Land"],
        subtypes: [],
      },
    });

    expect(
      abilityChoiceLabel(
        tapLandAction(1),
        object,
      ).label,
    ).toBe("Tap for {G}");
  });

  it("labels an atomic mana combination with every mana it produces", () => {
    const action = tapLandAction(1);
    action.data.selection.atomic_combination = ["White", "Blue"];

    expect(abilityChoiceLabel(action, makeGameObject()).label).toBe("Tap for {W}{U}");
  });

  it("labels the spell face cast action with the front-face name", () => {
    const object = makeGameObject();

    expect(
      abilityChoiceLabel(
        { type: "CastSpell", data: { object_id: 1, card_id: 100, targets: [] } },
        object,
      ),
    ).toEqual({ label: "Cast Bala Ged Recovery" });
  });

  it("labels a prepared copy cast with the prepare spell face name", () => {
    const object = makeGameObject({
      name: "Elite Interceptor",
      back_face: {
        name: "Rejoinder",
        power: null,
        toughness: null,
        card_types: { supertypes: [], core_types: ["Sorcery"], subtypes: [] },
        mana_cost: { type: "Cost", shards: ["White"], generic: 1 },
        keywords: [],
        abilities: [],
        color: ["White"],
      },
    });

    expect(
      abilityChoiceLabel({ type: "CastPreparedCopy", data: { source: 1 } }, object),
    ).toEqual({
      label: "Cast Rejoinder",
      description: "Cast a copy of Rejoinder. Elite Interceptor becomes unprepared.",
    });
  });

  it("labels the land play action with the land face name for spell-land MDFCs", () => {
    const object = makeGameObject();

    expect(
      abilityChoiceLabel(
        { type: "PlayLand", data: { object_id: 1, card_id: 100 } },
        object,
      ),
    ).toEqual({
      label: "Play Bala Ged Sanctuary",
      description: "Play this card as a land",
    });
  });
});

// ---------------------------------------------------------------------------
// Rows below cover the two authorities extracted in this change.
//
// EVIDENCE-LABEL CONVENTION (plan §7.2). A comment carrying a mutant claim is
// tagged, and the tag says what KIND of statement it is:
//   QUOTED    past-tense report — harness output copied verbatim from a run.
//   POINTER   names a sweep row / mutant id whose measurement lives in the
//             plan's evidence logs, not here.
//   MEASURED  a two-sided arm this PR ran at its own tip.
// An untagged comment is prose and is NOT evidence for anything.
//
// Expected payloads below are LITERALS. No row rebuilds its expectation by
// calling the function under test (or `collectObjectActions`) on the same
// source the implementation reads — a re-derived expectation co-varies with
// the implementation and cannot fail.
// ---------------------------------------------------------------------------

/** Freed from the Real: two NON-mana activated abilities on the Aura itself. */
const FREED_TAP: GameAction = {
  type: "ActivateAbility",
  data: { source_id: 408, ability_index: 0 },
};
const FREED_UNTAP: GameAction = {
  type: "ActivateAbility",
  data: { source_id: 408, ability_index: 1 },
};
const EQUIP_ACTION: GameAction = {
  type: "Equip",
  data: { equipment_id: 408, target_id: 401 },
};
const PREPARED_COPY: GameAction = { type: "CastPreparedCopy", data: { source: 408 } };

/** The Aura carries two non-mana abilities; index 2 is a mana ability. */
function freedFromTheReal(overrides: Partial<GameObject> = {}): GameObject {
  return makeGameObject({
    id: 408,
    zone: "Battlefield",
    name: "Freed from the Real",
    card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura"] },
    abilities: [
      { effect: { type: "Tap" } },
      { effect: { type: "Untap" } },
      { is_mana_ability: true, effect: { type: "Mana" } },
    ],
    ...overrides,
  });
}

const MANA_ABILITY: GameAction = {
  type: "ActivateAbility",
  data: { source_id: 408, ability_index: 2 },
};

function island(): GameObject {
  return makeGameObject({
    id: 500,
    zone: "Battlefield",
    name: "Island",
    card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Island"] },
    abilities: [{ is_mana_ability: true, effect: { type: "Mana" } }],
  });
}

const PRIORITY: WaitingFor = { type: "Priority", data: { player: 0 } };
const MANA_PAYMENT: WaitingFor = { type: "ManaPayment", data: { player: 0 } };
const DECLARE_BLOCKERS: WaitingFor = {
  type: "DeclareBlockers",
  data: { player: 0, valid_blocker_ids: [], valid_block_targets: {} },
};

function sorted(ids: Set<number>): number[] {
  return [...ids].sort((a, b) => a - b);
}

/**
 * Calls the activation authority the way a consumer does: the object's own id
 * drives BOTH the affordance lookup and the resolver argument.
 *
 * Every row declares both rings. The resolver's earlier signature carried only
 * the mana half, so the non-mana partition was merged unconditionally and a
 * cost-payment prompt offered activations the board itself refuses; rows that
 * pass `activate: false` are the ones that fail against that signature.
 */
function resolveFor(
  actions: GameAction[],
  object: GameObject,
  open: { activate: boolean; mana: boolean },
): ObjectActivation {
  const affordances: ActivationAffordances = {
    activatableObjectIds: new Set(open.activate ? [object.id] : []),
    manaTappableObjectIds: new Set(open.mana ? [object.id] : []),
  };
  return resolveObjectActivation(actions, object, affordances, object.id);
}

describe("deriveActivationAffordances", () => {
  // Two buckets the engine publishes: an Aura with two non-mana activations and
  // a land with a mana tap. Contents are asserted, never sizes — a size assert
  // passes under an "add every object" mutant.
  const objects: Record<string, GameObject> = {
    "408": freedFromTheReal(),
    "500": island(),
  };
  const legalActionsByObject: Record<string, GameAction[]> = {
    "408": [FREED_TAP, FREED_UNTAP],
    "500": [tapLandAction(500)],
  };

  // V14 — extraction parity. CR 113.3b: a non-mana activated ability is offered
  // only where the player has priority; the mana ring additionally opens during
  // the cost-payment states.
  it("offers the non-mana ring only at Priority, and the mana ring at every payment state", () => {
    const atPriority = deriveActivationAffordances(
      PRIORITY,
      true,
      legalActionsByObject,
      objects,
    );
    expect(sorted(atPriority.activatableObjectIds)).toEqual([408]);
    expect(sorted(atPriority.manaTappableObjectIds)).toEqual([500]);

    // POINTER: sweep row `V14 ManaPayment mana bucket` — the non-mana set must
    // stay EMPTY here, which is what the dropped-Priority-arm mutant flips.
    const atManaPayment = deriveActivationAffordances(
      MANA_PAYMENT,
      true,
      legalActionsByObject,
      objects,
    );
    expect(sorted(atManaPayment.activatableObjectIds)).toEqual([]);
    expect(sorted(atManaPayment.manaTappableObjectIds)).toEqual([500]);

    // CR 118.12a: a disjunctive unless-cost enables the same mana input as a
    // plain UnlessPayment.
    for (const waitingFor of [
      { type: "UnlessPayment", data: { player: 0, cost: {}, pending_effect: null } },
      { type: "UnlessPaymentChooseCost", data: { player: 0, costs: [], pending_effect: null } },
    ] as unknown as WaitingFor[]) {
      const derived = deriveActivationAffordances(waitingFor, true, legalActionsByObject, objects);
      expect(sorted(derived.activatableObjectIds)).toEqual([]);
      expect(sorted(derived.manaTappableObjectIds)).toEqual([500]);
    }
  });

  // V4's viewmodel half — the timing axis. A non-Priority combat state offers
  // neither ring even though both buckets are populated.
  it("offers nothing at DeclareBlockers even with both buckets populated", () => {
    const derived = deriveActivationAffordances(
      DECLARE_BLOCKERS,
      true,
      legalActionsByObject,
      objects,
    );
    expect(sorted(derived.activatableObjectIds)).toEqual([]);
    expect(sorted(derived.manaTappableObjectIds)).toEqual([]);
  });

  // The seat axis, independent of timing: same Priority prompt, but this viewer
  // is not the player the engine is waiting on.
  it("offers nothing at Priority when the viewer cannot act for the waiting state", () => {
    const derived = deriveActivationAffordances(
      PRIORITY,
      false,
      legalActionsByObject,
      objects,
    );
    expect(sorted(derived.activatableObjectIds)).toEqual([]);
    expect(sorted(derived.manaTappableObjectIds)).toEqual([]);
  });

  // NEGATIVE CONTROL. Pre-init (`objects` undefined) and an id present in the
  // bucket but absent from the objects map must both drop out — never crash,
  // never admit an unknown id.
  it("drops ids with no object, and returns empty sets before the state loads", () => {
    const noObjects = deriveActivationAffordances(PRIORITY, true, legalActionsByObject, undefined);
    expect(sorted(noObjects.activatableObjectIds)).toEqual([]);
    expect(sorted(noObjects.manaTappableObjectIds)).toEqual([]);

    const staleBucket = deriveActivationAffordances(
      PRIORITY,
      true,
      { ...legalActionsByObject, "999": [FREED_TAP] },
      objects,
    );
    expect(sorted(staleBucket.activatableObjectIds)).toEqual([408]);
  });
});

describe("resolveObjectActivation", () => {
  // V15 (D1) — an empty merged list is `none`, NOT an empty choice modal.
  // POINTER: sweep row `C9 empty bucket`, mutant `emptyModal`. Labelled
  // DROP-ONLY in the plan (§7.1): the expected verdict IS the degenerate value,
  // so no always-mutant can be visible on this row and it is counted as
  // drop-side coverage only. Its consumer-side pair is the V25 module.
  it("returns none for an empty bucket instead of opening an empty modal", () => {
    expect(resolveFor([], freedFromTheReal(), { activate: true, mana: false })).toEqual({ kind: "none" });
  });

  // POINTER: sweep row `C10 mana-only canTap=F`, also DROP-ONLY. Dropping the
  // mana action when the mana ring is closed must yield `none`, not a modal
  // offering an action the player cannot pay with.
  it("returns none when the only action is mana and the mana ring is closed", () => {
    expect(resolveFor([tapLandAction(500)], island(), { activate: true, mana: false })).toEqual({
      kind: "none",
    });
  });

  // V18 (D3) — a lone MANA action that consumes its source must NOT auto-fire.
  // POINTER: sweep row `C7 lone MANA consumes canTap=T`, mutants `noGate506`
  // and `manaCarveOut`. This is a contract fix, not a live bug: a battlefield
  // permanent cannot carry `consumes_source` today.
  it("routes a lone consuming mana ability through the confirmation modal", () => {
    const object = freedFromTheReal({
      abilities: [
        { effect: { type: "Tap" } },
        { effect: { type: "Untap" } },
        { is_mana_ability: true, consumes_source: true, effect: { type: "Mana" } },
      ],
    });
    expect(resolveFor([MANA_ABILITY], object, { activate: true, mana: true })).toEqual({
      kind: "choose",
      actions: [MANA_ABILITY],
    });
  });

  // V18b — the benign sibling, so V18 cannot pass by "mana always opens the
  // modal". POINTER: sweep row `C4b`; PAIR-ONLY per §7.1 (its always-side
  // mutant is `alwaysChoose`; no drop-the-fix mutant is visible on it).
  it("still auto-dispatches a lone non-consuming mana ability", () => {
    expect(resolveFor([MANA_ABILITY], freedFromTheReal(), { activate: true, mana: true })).toEqual({
      kind: "dispatch",
      action: MANA_ABILITY,
    });
  });

  // V19 (CR 605.1a) — the mana/non-mana partition is driven by the mana ring.
  // POINTER: sweep rows `C6 mana+nonmana canTap=T` / `C8 mana+nonmana canTap=F`,
  // mutant `M2`.
  it("merges mana actions after the non-mana ones only when the mana ring is open", () => {
    const object = freedFromTheReal();
    expect(resolveFor([FREED_TAP, MANA_ABILITY], object, { activate: true, mana: true })).toEqual({
      kind: "choose",
      actions: [FREED_TAP, MANA_ABILITY],
    });
    // A closed mana ring drops the mana action entirely, leaving a lone benign
    // non-mana action — which auto-dispatches.
    expect(resolveFor([FREED_TAP, MANA_ABILITY], object, { activate: true, mana: false })).toEqual({
      kind: "dispatch",
      action: FREED_TAP,
    });
  });

  // V19b — the TYPE-based mana branch (`TapLandForMana` / `TapForConvoke`) is
  // honored, not just the `is_mana_ability` flag. POINTER: sweep row
  // `C13b lone TapLandForMana canTap=F` (DROP-ONLY), mutant `M2`; the canTap=T
  // half is sweep row `C13`, PAIR-ONLY.
  it("classifies TapLandForMana and TapForConvoke by action type", () => {
    const land = island();
    const tapLand = tapLandAction(500);
    expect(resolveFor([tapLand], land, { activate: true, mana: false })).toEqual({ kind: "none" });
    expect(resolveFor([tapLand], land, { activate: true, mana: true })).toEqual({
      kind: "dispatch",
      action: tapLand,
    });

    const convoke: GameAction = {
      type: "TapForConvoke",
      data: { object_id: 500, mana_type: "Blue" },
    };
    expect(resolveFor([convoke], land, { activate: true, mana: false })).toEqual({ kind: "none" });
  });

  // V19c (CR 113.3b) — the `keywords` partition arm is load-bearing: an Equip
  // is neither mana nor `ActivateAbility`, so without that arm it vanishes from
  // the merged list. POINTER: sweep row `C11 lone Equip`, mutant `M1`.
  it("surfaces a keyword activation that is neither mana nor ActivateAbility", () => {
    expect(resolveFor([EQUIP_ACTION], freedFromTheReal(), { activate: true, mana: false })).toEqual({
      kind: "dispatch",
      action: EQUIP_ACTION,
    });
  });

  // V20c2 — a mixed-group bucket keeps `[ability, keyword]` order. POINTER:
  // sweep row `E1c ability+Equip (mixed groups)`, mutants `M1` / `revConcat` /
  // `allReverse`. Order is asserted, so "any two actions" fails.
  it("keeps [ability, keyword] order in a mixed bucket", () => {
    expect(
      resolveFor([FREED_TAP, EQUIP_ACTION], freedFromTheReal(), { activate: true, mana: false }),
    ).toEqual({ kind: "choose", actions: [FREED_TAP, EQUIP_ACTION] });
  });

  // V19d — both partition arms at once. POINTER: sweep row
  // `C14 Equip+mana canTap=T`, mutants `M1` AND `M2`.
  it("orders [keyword, mana] when both partitions fire", () => {
    expect(
      resolveFor([MANA_ABILITY, EQUIP_ACTION], freedFromTheReal(), { activate: true, mana: true }),
    ).toEqual({ kind: "choose", actions: [EQUIP_ACTION, MANA_ABILITY] });
  });

  // V19e — `CastPreparedCopy` reaches the keyword arm AND the #506 gate, so a
  // lone prepared copy is offered explicitly instead of firing on one click.
  // POINTER: sweep row `C15 lone CastPreparedCopy`, mutants `M1` + `noGate506`.
  it("offers a lone CastPreparedCopy through the modal rather than auto-casting", () => {
    expect(resolveFor([PREPARED_COPY], freedFromTheReal(), { activate: true, mana: false })).toEqual({
      kind: "choose",
      actions: [PREPARED_COPY],
    });
  });

  // V20d (D5) — DISCLOSED order change. A bucket the engine publishes
  // mana-first comes back non-mana-first after partitioning. POINTER: sweep row
  // `E2 mana-first + non-mana canTap=T`, mutants `M2` / `revConcat` /
  // `rawBucket` / `allReverse`.
  it("deliberately reorders a mana-first bucket to [non-mana, mana]", () => {
    const rawBucket: GameAction[] = [MANA_ABILITY, FREED_TAP];
    expect(resolveFor(rawBucket, freedFromTheReal(), { activate: true, mana: true })).toEqual({
      kind: "choose",
      actions: [FREED_TAP, MANA_ABILITY],
    });
    // The raw engine order is the thing that changed — assert it explicitly so
    // this row cannot be read as "order happens to be preserved".
    expect(rawBucket).toEqual([MANA_ABILITY, FREED_TAP]);
  });

  // The user's bucket, at the viewmodel layer: Freed from the Real publishes
  // TWO non-mana activated abilities, so the click must open the chooser with
  // both, in engine order.
  it("offers both of an Aura's activated abilities in engine order", () => {
    expect(
      resolveFor([FREED_TAP, FREED_UNTAP], freedFromTheReal(), { activate: true, mana: false }),
    ).toEqual({ kind: "choose", actions: [FREED_TAP, FREED_UNTAP] });
  });

  // ------------------------------------------------------------------------
  // The non-mana ring is a GATE, not decoration (CR 113.3b).
  //
  // MEASURED (two-sided, this PR). DROP side — restoring the unconditional
  // merge the previous 3-argument signature had (`if (true)` in place of the
  // ring test) fails the three rows below; the first flips with
  // `AssertionError: expected { kind: 'choose', …(1) } to deeply equal
  // { kind: 'none' }` — the exact `[P4 ManaPayment] verdict=choose[NON-mana,
  // mana]` the review measured, byte-identical to the `[P4 Priority]` verdict
  // at a state where `deriveActivationAffordances` had already closed the
  // non-mana ring. ALWAYS side — forcing the ring closed (`if (false)`) fails
  // ten rows, including every positive control below.
  // ------------------------------------------------------------------------
  it("drops the non-mana partition when the activation ring is closed", () => {
    const aura = freedFromTheReal();
    // A cost-payment prompt: `deriveActivationAffordances` opens the mana ring
    // only. The two non-mana abilities must not be offered.
    expect(resolveFor([FREED_TAP, FREED_UNTAP], aura, { activate: false, mana: true })).toEqual({
      kind: "none",
    });
    // …and neither ring open is `none` too — the object is simply not offered.
    expect(resolveFor([FREED_TAP, FREED_UNTAP], aura, { activate: false, mana: false })).toEqual({
      kind: "none",
    });
    // POSITIVE CONTROL (reach guard): the identical bucket with the non-mana
    // ring OPEN is offered, so the two assertions above cannot pass by "this
    // fixture is never offered".
    expect(resolveFor([FREED_TAP, FREED_UNTAP], aura, { activate: true, mana: true })).toEqual({
      kind: "choose",
      actions: [FREED_TAP, FREED_UNTAP],
    });
  });

  // The mixed bucket at a cost-payment prompt — the shape the review measured.
  // Only the mana action survives, and being alone and benign it auto-fires
  // instead of opening a modal that offers a non-mana activation.
  it("offers only the mana action at a cost-payment prompt", () => {
    const aura = freedFromTheReal();
    expect(
      resolveFor([FREED_TAP, MANA_ABILITY, EQUIP_ACTION], aura, { activate: false, mana: true }),
    ).toEqual({ kind: "dispatch", action: MANA_ABILITY });
    // Positive control: with the non-mana ring open the same bucket offers all
    // three, in `[ability, keyword, mana]` order.
    expect(
      resolveFor([FREED_TAP, MANA_ABILITY, EQUIP_ACTION], aura, { activate: true, mana: true }),
    ).toEqual({ kind: "choose", actions: [FREED_TAP, EQUIP_ACTION, MANA_ABILITY] });
  });

  // HOSTILE FIXTURE — membership is keyed by the id passed in, never by "the
  // set is non-empty". Calls the real 4-argument signature directly (not via
  // `resolveFor`), with both rings populated for a DIFFERENT object.
  it("reads both rings by object id, not by set emptiness", () => {
    const aura = freedFromTheReal();
    const otherId = aura.id + 1;
    const affordances: ActivationAffordances = {
      activatableObjectIds: new Set([otherId]),
      manaTappableObjectIds: new Set([otherId]),
    };
    expect(
      resolveObjectActivation([FREED_TAP, MANA_ABILITY], aura, affordances, aura.id),
    ).toEqual({ kind: "none" });
    // Positive control: the same non-empty sets, queried for the id they name.
    expect(
      resolveObjectActivation([FREED_TAP, MANA_ABILITY], aura, affordances, otherId),
    ).toEqual({ kind: "choose", actions: [FREED_TAP, MANA_ABILITY] });
  });
});
