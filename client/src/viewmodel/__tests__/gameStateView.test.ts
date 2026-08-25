import { describe, expect, it } from "vitest";

import type {
  GameAction,
  GameObject,
  GameState,
  PlayerId,
  TargetRef,
  WaitingFor,
} from "../../adapter/types";
import {
  buildGameObject,
  buildGameObjectWithCoreTypes,
  buildObjectMap,
} from "../../test/factories/gameObjectFactory";
import {
  buildCopyTargetSlot,
  buildGameState,
  buildGameStateWithoutSeatOrder,
  buildPendingCast,
  buildPlayers,
  buildTargetSelectionProgress,
  buildTargetSelectionSlot,
  copyRetargetWaitingForFactory,
  retargetChoiceWaitingForFactory,
  returnAsAuraTargetWaitingForFactory,
  targetSelectionWaitingForFactory,
  triggerTargetSelectionWaitingForFactory,
} from "../../test/factories/gameStateFactory";
import {
  boardChoiceSelectedPower,
  buildBoardChoiceAction,
  canConfirmBoardChoice,
  getBattlefieldSacrificeChoice,
  getBoardChoiceView,
  getCastableZoneViewerTarget,
  getOpponentIds,
  getSeatCount,
  getVisibleBoardPlayerIds,
  getWaitingForClickTargetRefs,
  getWaitingForObjectChoiceIds,
  getWaitingForPlayerChoiceIds,
  isFaceDownExileCardVisibleToViewer,
  isOneOnOne,
  isSplitBoardActive,
  resolveMultiplayerBoardLayout,
  resolveFocusedOpponent,
  shouldRenderFocusedOpponentTopRow,
} from "../gameStateView";

function makeState(seatOrder: PlayerId[], eliminated: PlayerId[] = []): GameState {
  return buildGameState({
    seat_order: seatOrder,
    eliminated_players: eliminated,
    players: buildPlayers(seatOrder),
  });
}

describe("getSeatCount", () => {
  it("returns the seat_order length for a 2-player game", () => {
    expect(getSeatCount(makeState([0, 1]))).toBe(2);
  });

  it("returns the seat_order length for a 4-player game", () => {
    expect(getSeatCount(makeState([0, 1, 2, 3]))).toBe(4);
  });

  it("stays stable after eliminations (seat_order is not pruned)", () => {
    expect(getSeatCount(makeState([0, 1, 2, 3], [1, 2]))).toBe(4);
  });

  it("falls back to players.length when seat_order is absent", () => {
    const state = buildGameStateWithoutSeatOrder({ players: buildPlayers([0, 1, 2]) });
    expect(getSeatCount(state)).toBe(3);
  });

  it("returns 0 for a null state", () => {
    expect(getSeatCount(null)).toBe(0);
  });
});

describe("isOneOnOne", () => {
  // The bug that motivates this helper: GameBoard and OpponentHud derived
  // "is this 1v1?" from different inputs (live opponents vs. seat count).
  // In a 4-player Commander game with two eliminations, the derivations
  // disagreed and the multi-tab rail got crammed into the 1v1 inline-pill
  // slot. These cases lock the boundary so that can't recur.

  it("is true for a fresh 2-player game", () => {
    expect(isOneOnOne(makeState([0, 1]))).toBe(true);
  });

  it("is false for a fresh 4-player game", () => {
    expect(isOneOnOne(makeState([0, 1, 2, 3]))).toBe(false);
  });

  it("stays false for a 4-player game with 1 live opponent (regression case)", () => {
    // Player 0's perspective: opponents 1 and 2 eliminated, only 3 alive.
    expect(isOneOnOne(makeState([0, 1, 2, 3], [1, 2]))).toBe(false);
  });

  it("stays false for a 4-player game with all opponents eliminated", () => {
    expect(isOneOnOne(makeState([0, 1, 2, 3], [1, 2, 3]))).toBe(false);
  });

  it("stays true for a 2-player game with the opponent eliminated", () => {
    // GameOver mounts on the same state — the helper just needs to not
    // flip layouts on the way there.
    expect(isOneOnOne(makeState([0, 1], [1]))).toBe(true);
  });

  it("returns false for a null state", () => {
    expect(isOneOnOne(null)).toBe(false);
  });
});

describe("resolveFocusedOpponent", () => {
  it("returns the explicit focus when that opponent is still live", () => {
    expect(resolveFocusedOpponent(3, [1, 3])).toBe(3);
  });

  it("falls back to the first live opponent when focus is eliminated", () => {
    expect(resolveFocusedOpponent(1, [3])).toBe(3);
  });

  it("returns null when no live opponents remain", () => {
    expect(resolveFocusedOpponent(1, [])).toBeNull();
  });
});

describe("getVisibleBoardPlayerIds", () => {
  it("returns local and focused live opponent in focused multiplayer", () => {
    expect(getVisibleBoardPlayerIds(makeState([0, 1, 2, 3]), 0, 2, "focused")).toEqual([0, 2]);
  });

  it("falls back to the first live opponent in focused multiplayer", () => {
    expect(getVisibleBoardPlayerIds(makeState([0, 1, 2, 3]), 0, null, "focused")).toEqual([0, 1]);
  });

  it("returns local and all live opponents in split multiplayer", () => {
    expect(getVisibleBoardPlayerIds(makeState([0, 1, 2, 3]), 0, 2, "split")).toEqual([0, 1, 2, 3]);
  });

  it("excludes eliminated opponents in split multiplayer", () => {
    expect(getVisibleBoardPlayerIds(makeState([0, 1, 2, 3], [2]), 0, 2, "split")).toEqual([0, 1, 3]);
  });

  it("returns an empty list for null state", () => {
    expect(getVisibleBoardPlayerIds(null, 0, 1, "split")).toEqual([]);
  });

  it("keeps 1v1 unchanged even when split is selected", () => {
    expect(getVisibleBoardPlayerIds(makeState([0, 1]), 0, null, "split")).toEqual([0, 1]);
  });
});

describe("split board ownership helpers", () => {
  it("resolves auto by viewport while honoring explicit multiplayer choices", () => {
    expect(resolveMultiplayerBoardLayout("auto", 3, true)).toBe("focused");
    expect(resolveMultiplayerBoardLayout("auto", 3, false)).toBe("split");
    expect(resolveMultiplayerBoardLayout("split", 3, true)).toBe("split");
    expect(resolveMultiplayerBoardLayout("focused", 3, false)).toBe("focused");
    expect(resolveMultiplayerBoardLayout("split", 2, false)).toBe("focused");
  });

  it("activates split layout only for 3+ player games", () => {
    expect(isSplitBoardActive("split", 4)).toBe(true);
    expect(isSplitBoardActive("split", 2)).toBe(false);
    expect(isSplitBoardActive("focused", 4)).toBe(false);
  });

  it("suppresses the focused opponent top row only in active split mode", () => {
    expect(shouldRenderFocusedOpponentTopRow("split", 4)).toBe(false);
    expect(shouldRenderFocusedOpponentTopRow("split", 2)).toBe(true);
    expect(shouldRenderFocusedOpponentTopRow("focused", 4)).toBe(true);
  });
});

describe("getWaitingForObjectChoiceIds", () => {
  it("returns valid_tokens for PopulateChoice", () => {
    expect(
      getWaitingForObjectChoiceIds({
        type: "PopulateChoice",
        data: { player: 0, source_id: 1, valid_tokens: [10, 11] },
      }),
    ).toEqual([10, 11]);
  });

  // PairChoice is modal-resolved (PairChoiceModal dispatches ChoosePair), so it
  // must NOT seed board-clickable object glow. The engine rejects ChooseTarget
  // for PairChoice, so a board click would dead-end. Mirrors CrewVehicle /
  // StationTarget / SaddleMount, which are likewise absent here.
  it("returns [] for PairChoice (modal-only, not board-clickable)", () => {
    expect(
      getWaitingForObjectChoiceIds({
        type: "PairChoice",
        data: { player: 0, source_id: 1, choices: [20, 21, 22] },
      }),
    ).toEqual([]);
  });

  // CR 707.9: Copy Enchantment's copy pool arrives as `CopyTargetChoice`.
  // Every surface that can offer one of these objects — battlefield card or
  // the player-attached-Aura dialog — must read the pool from here.
  it("returns valid_targets for CopyTargetChoice", () => {
    expect(
      getWaitingForObjectChoiceIds({
        type: "CopyTargetChoice",
        data: { player: 0, source_id: 1, valid_targets: [30, 31] },
      }),
    ).toEqual([30, 31]);
  });
});

describe("getBattlefieldSacrificeChoice", () => {
  it("returns engine-provided battlefield sacrifice candidates", () => {
    expect(
      getBattlefieldSacrificeChoice({
        type: "EffectZoneChoice",
        data: {
          player: 0,
          cards: [10, 11],
          count: 2,
          min_count: 1,
          up_to: true,
          source_id: 99,
          effect_kind: "Sacrifice",
          zone: "Battlefield",
          destination: null,
        },
      }),
    ).toEqual({
      objectIds: [10, 11],
      count: 2,
      minCount: 1,
      upTo: true,
    });
  });

  it("returns ward sacrifice candidates", () => {
    expect(
      getBattlefieldSacrificeChoice({
        type: "WardSacrificeChoice",
        data: {
          player: 0,
          permanents: [20, 21],
          pending_effect: {},
          remaining: 1,
        },
      }),
    ).toEqual({
      objectIds: [20, 21],
      count: 1,
      minCount: 1,
      upTo: false,
    });
  });

  it("does not treat non-sacrifice zone choices as board sacrifice choices", () => {
    expect(
      getBattlefieldSacrificeChoice({
        type: "EffectZoneChoice",
        data: {
          player: 0,
          cards: [30],
          count: 1,
          source_id: 99,
          effect_kind: "ReturnToHand",
          zone: "Battlefield",
          destination: "Hand",
        },
      }),
    ).toBeNull();
  });
});

describe("getBoardChoiceView", () => {
  it("maps BlightChoice to one confirmed creature selection", () => {
    const choice = getBoardChoiceView({
      type: "BlightChoice",
      data: {
        player: 0,
        counters: 3,
        creatures: [10, 11],
        pending_cast: buildPendingCast({ object_id: 99 }),
      },
    });

    expect(choice).toMatchObject({
      player: 0,
      objectIds: [10, 11],
      intent: "blight",
      selection: { type: "exactCount", count: 1 },
      response: { type: "SelectCards" },
      sourceId: 99,
      cancelAction: { type: "CancelCast" },
    });
    expect(choice).not.toBeNull();
    if (!choice) return;
    expect(canConfirmBoardChoice(choice, [10], undefined)).toBe(true);
  });

  it("maps PayCost ReturnToHand to a confirmed board choice", () => {
    const choice = getBoardChoiceView(
      {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "ReturnToHand" },
          choices: [4, 5],
          count: 1,
          min_count: 1,
          resume: {
            type: "Spell",
            Spell: {
              object_id: 99,
              card_id: 990,
              ability: { targets: [] },
              cost: { type: "NoCost" },
            },
          },
        },
      },
      buildObjectMap(
        buildGameObject({ id: 4, zone: "Battlefield" }),
        buildGameObject({ id: 5, zone: "Battlefield" }),
      ),
    );

    expect(choice).toMatchObject({
      player: 0,
      objectIds: [4, 5],
      intent: "return",
      selection: { type: "exactCount", count: 1 },
      response: { type: "SelectCards" },
      sourceId: 99,
      cancelAction: { type: "CancelCast" },
    });
  });

  it("maps battlefield untap effects to board card selection", () => {
    const choice = getBoardChoiceView({
      type: "EffectZoneChoice",
      data: {
        player: 1,
        cards: [10, 11],
        count: 2,
        min_count: 1,
        up_to: true,
        source_id: 9,
        effect_kind: "Untap",
        zone: "Battlefield",
      },
    });

    expect(choice).toMatchObject({
      player: 1,
      objectIds: [10, 11],
      intent: "untap",
      selection: { type: "rangeCount", min: 1, max: 2 },
      response: { type: "SelectCards" },
    });
  });

  it("maps capped untap subsets to a zero-to-max board selection", () => {
    const choice = getBoardChoiceView({
      type: "ChooseUntapSubset",
      data: { player: 1, group: [10, 11], max: 1 },
    });

    expect(choice).toMatchObject({
      player: 1,
      objectIds: [10, 11],
      intent: "untap",
      selection: { type: "rangeCount", min: 0, max: 1 },
      response: { type: "SelectCards" },
    });
    expect(choice && buildBoardChoiceAction(choice, [10])).toEqual({
      type: "SelectCards",
      data: { cards: [10] },
    });
  });

  it("maps an untap decision to the first candidate and typed choose action", () => {
    const choice = getBoardChoiceView({
      type: "UntapChoice",
      data: { player: 1, candidates: [10, 11] },
    });

    expect(choice).toMatchObject({
      player: 1,
      objectIds: [10],
      intent: "untap",
      selection: { type: "single", immediate: true },
      response: { type: "ChooseUntap", objectId: 10 },
      skipAction: { type: "ChooseUntap", data: { object_id: 10, untap: false } },
      skipLabel: "keepTapped",
    });
    expect(choice && buildBoardChoiceAction(choice, [10])).toEqual({
      type: "ChooseUntap",
      data: { object_id: 10, untap: true },
    });
  });

  it("does not surface an empty untap decision", () => {
    expect(getBoardChoiceView({
      type: "UntapChoice",
      data: { player: 1, candidates: [] },
    })).toBeNull();
  });

  it("builds CrewVehicle actions and gates by selected total power", () => {
    const choice = getBoardChoiceView({
      type: "CrewVehicle",
      data: {
        player: 0,
        vehicle_id: 30,
        crew_power: 4,
        eligible_creatures: [10, 11],
        contributions: [2, 3],
      },
    });
    const objects = buildObjectMap(
      buildGameObject({ id: 10, power: 2 }),
      buildGameObject({ id: 11, power: 3 }),
    );

    expect(choice).not.toBeNull();
    if (!choice) return;
    expect(boardChoiceSelectedPower(choice, [10], objects)).toBe(2);
    expect(canConfirmBoardChoice(choice, [10], objects)).toBe(false);
    expect(canConfirmBoardChoice(choice, [10, 11], objects)).toBe(true);
    expect(buildBoardChoiceAction(choice, [10, 11])).toEqual({
      type: "CrewVehicle",
      data: { vehicle_id: 30, creature_ids: [10, 11] },
    });
    expect(choice.cancelAction).toEqual({ type: "CancelCast" });
  });

  // Regression: a Pilot token (Shorikai) has printed power 1 but crews "as though
  // its power were 2 greater" (contribution 3). The UI must gate on the engine's
  // contribution, not raw power, so a lone Pilot satisfies Crew 3. Summing raw
  // power gave 1 < 3 and wrongly blocked the crew ("crews for just 1").
  it("gates CrewVehicle by the engine contribution, not printed power", () => {
    const choice = getBoardChoiceView({
      type: "CrewVehicle",
      data: {
        player: 0,
        vehicle_id: 30,
        crew_power: 3,
        eligible_creatures: [10],
        contributions: [3],
      },
    });
    const objects = buildObjectMap(buildGameObject({ id: 10, power: 1 }));

    expect(choice).not.toBeNull();
    if (!choice) return;
    // Printed power is 1, but the engine says this creature contributes 3.
    expect(boardChoiceSelectedPower(choice, [10], objects)).toBe(3);
    expect(canConfirmBoardChoice(choice, [10], objects)).toBe(true);
  });

  it("gates SaddleMount by the engine contribution, not printed power", () => {
    const choice = getBoardChoiceView({
      type: "SaddleMount",
      data: {
        player: 0,
        mount_id: 40,
        saddle_power: 3,
        eligible_creatures: [10],
        contributions: [3],
      },
    });
    const objects = buildObjectMap(buildGameObject({ id: 10, power: 1 }));

    expect(choice).not.toBeNull();
    if (!choice) return;
    expect(boardChoiceSelectedPower(choice, [10], objects)).toBe(3);
    expect(canConfirmBoardChoice(choice, [10], objects)).toBe(true);
    expect(buildBoardChoiceAction(choice, [10])).toEqual({
      type: "SaddleMount",
      data: { mount_id: 40, creature_ids: [10] },
    });
  });

  it("sums raw power for Slaughter keep sets so negative-power creatures lower the total", () => {
    const choice = getBoardChoiceView({
      type: "KeepWithinTotalPowerChoice",
      data: {
        player: 0,
        target_player: 0,
        eligible: [10, 11],
        cap: 4,
        source_id: 50,
        remaining_players: [],
        all_kept: [],
        scoped_players: [0],
      },
    });
    const objects = buildObjectMap(
      buildGameObject({ id: 10, power: 5 }),
      buildGameObject({ id: 11, power: -1 }),
    );

    expect(choice).not.toBeNull();
    if (!choice) return;
    expect(choice.selection).toEqual({ type: "totalPowerAtMost", power: 4 });
    // Raw sum mirrors the engine's CR 208.3 total: 5 + (-1) = 4, not a
    // positive-clamped 6 that would wrongly disable confirm.
    expect(boardChoiceSelectedPower(choice, [10, 11], objects)).toBe(4);
    expect(canConfirmBoardChoice(choice, [10, 11], objects)).toBe(true);
    // Keeping only the 5-power creature exceeds the cap of 4.
    expect(canConfirmBoardChoice(choice, [10], objects)).toBe(false);
  });

  it("renders the engine-provided exact keeper requirement without client-side capping", () => {
    const choice = getBoardChoiceView({
      type: "KeepExactPermanentsChoice",
      data: {
        player: 0,
        target_player: 0,
        eligible: [10, 11],
        required_count: 5,
        source_id: 50,
        remaining_players: [],
        all_kept: [],
        scoped_players: [0],
      },
    });

    expect(choice).not.toBeNull();
    expect(choice?.selection).toEqual({ type: "exactCount", count: 5 });
  });

  it("maps simple StationTarget and Ring-bearer choices to immediate single actions", () => {
    const station = getBoardChoiceView({
      type: "StationTarget",
      data: {
        player: 0,
        spacecraft_id: 20,
        eligible_creatures: [7],
      },
    });
    const ringBearer = getBoardChoiceView({
      type: "ChooseRingBearer",
      data: {
        player: 0,
        candidates: [12],
      },
    });

    expect(station?.selection).toEqual({ type: "single", immediate: true });
    expect(station && buildBoardChoiceAction(station, [7])).toEqual({
      type: "ActivateStation",
      data: { spacecraft_id: 20, creature_id: 7 },
    });
    expect(ringBearer && buildBoardChoiceAction(ringBearer, [12])).toEqual({
      type: "ChooseRingBearer",
      data: { target: 12 },
    });
  });

  it("keeps RemoveCounter costs modal-only", () => {
    expect(
      getBoardChoiceView({
        type: "PayCost",
        data: {
          player: 0,
          kind: {
            type: "RemoveCounter",
            counter_type: { type: "Any" },
            count: 1,
            selection: "SingleObject",
          },
          choices: [4],
          count: 1,
          min_count: 1,
          resume: { type: "ManaAbility", ManaAbility: {} },
        },
      }),
    ).toBeNull();
  });

  it("maps resolution TapCreatures PayCost to a non-cancellable board choice", () => {
    const waitingFor: WaitingFor = {
      type: "PayCost",
      data: {
        player: 0,
        kind: { type: "TapCreatures" },
        choices: [4, 5],
        count: 2,
        min_count: 2,
        resume: { type: "Resolution" },
      },
    };

    const choice = getBoardChoiceView(
      waitingFor,
      buildObjectMap(
        buildGameObject({ id: 4, zone: "Battlefield" }),
        buildGameObject({ id: 5, zone: "Battlefield" }),
      ),
    );

    expect(choice).toMatchObject({
      player: 0,
      objectIds: [4, 5],
      intent: "tap",
      selection: { type: "exactCount", count: 2 },
      response: { type: "SelectCards" },
      cancelAction: undefined,
    });
    expect(choice && buildBoardChoiceAction(choice, [4, 5])).toEqual({
      type: "SelectCards",
      data: { cards: [4, 5] },
    });
  });

  it("keeps PayCost choices modal-only unless every candidate is on the battlefield", () => {
    const waitingFor: WaitingFor = {
      type: "PayCost",
      data: {
        player: 0,
        kind: { type: "ExilePermanent", filter: null },
        choices: [4, 5],
        count: 1,
        min_count: 1,
        resume: { type: "ManaAbility", ManaAbility: {} },
      },
    };

    expect(
      getBoardChoiceView(
        waitingFor,
        buildObjectMap(
          buildGameObject({ id: 4, zone: "Battlefield" }),
          buildGameObject({ id: 5, zone: "Graveyard" }),
        ),
      ),
    ).toBeNull();
  });
});

describe("getCastableZoneViewerTarget", () => {
  const castAction: GameAction = {
    type: "CastSpell",
    data: { object_id: 7, card_id: 700, targets: [] },
  };
  const activateAction: GameAction = {
    type: "ActivateAbility",
    data: { source_id: 7, ability_index: 0 },
  };

  function makeGraveyardObject(id: number): GameObject {
    return buildGameObjectWithCoreTypes(["Instant"], {
      id,
      card_id: 700 + id,
      zone: "Graveyard",
      name: `Spell ${id}`,
      mana_cost: { type: "Cost", shards: ["Red"], generic: 0 },
      keywords: ["Retrace"],
      color: ["Red"],
      base_keywords: ["Retrace"],
      base_color: ["Red"],
      entered_battlefield_turn: null,
    });
  }

  it("returns the graveyard pile when Priority surfaces cast actions there", () => {
    const objects = buildObjectMap(makeGraveyardObject(7), makeGraveyardObject(8));
    expect(
      getCastableZoneViewerTarget(
        { type: "Priority", data: { player: 0 } },
        objects,
        {
          "7": [castAction],
          "8": [{ ...castAction, data: { ...castAction.data, object_id: 8 } }],
        },
      ),
    ).toEqual({ zone: "graveyard", playerId: 0, objectIds: [7, 8] });
  });

  it("returns stable object ids for castable pile identity", () => {
    const objects = {
      7: makeGraveyardObject(7),
      8: makeGraveyardObject(8),
    };
    expect(
      getCastableZoneViewerTarget(
        { type: "Priority", data: { player: 0 } },
        objects,
        {
          "8": [{ ...castAction, data: { ...castAction.data, object_id: 8 } }],
          "7": [castAction],
        },
      )?.objectIds,
    ).toEqual([7, 8]);
  });

  it("returns null when castable cards span multiple zone piles", () => {
    const objects = {
      7: makeGraveyardObject(7),
      9: { ...makeGraveyardObject(9), zone: "Exile" as const, owner: 0 },
    };
    expect(
      getCastableZoneViewerTarget(
        { type: "Priority", data: { player: 0 } },
        objects,
        {
          "7": [castAction],
          "9": [{ ...castAction, data: { ...castAction.data, object_id: 9 } }],
        },
      ),
    ).toBeNull();
  });

  it("returns null outside Priority", () => {
    const objects = { 7: makeGraveyardObject(7) };
    expect(
      getCastableZoneViewerTarget(
        { type: "CastingVariantChoice", data: { player: 0, object_id: 7, card_id: 700, options: [] } },
        objects,
        { "7": [castAction] },
      ),
    ).toBeNull();
  });

  it("ignores graveyard objects without play or cast actions", () => {
    const objects = { 7: makeGraveyardObject(7) };
    expect(
      getCastableZoneViewerTarget(
        { type: "Priority", data: { player: 0 } },
        objects,
        { "7": [activateAction] },
      ),
    ).toBeNull();
  });
});

describe("getOpponentIds", () => {
  it("excludes the perspective player and eliminated players", () => {
    expect(getOpponentIds(makeState([0, 1, 2, 3], [2]), 0)).toEqual([1, 3]);
  });

  it("returns an empty array in a 2-player game with the opponent eliminated", () => {
    // This is the regression edge case the 1v1 branch in GameBoard now
    // guards against — `opponents[0]` is undefined here, and the layout
    // must not index `gameState.players[undefined]`.
    expect(getOpponentIds(makeState([0, 1], [1]), 0)).toEqual([]);
  });
});

describe("isFaceDownExileCardVisibleToViewer", () => {
  function faceDownObject(overrides: Partial<GameObject> = {}): GameObject {
    return buildGameObjectWithCoreTypes(["Creature"], {
      id: 2,
      card_id: 200,
      owner: 1,
      controller: 1,
      zone: "Exile",
      face_down: true,
      name: "Ghalta, Primal Hunter",
      mana_cost: { type: "Cost", shards: [], generic: 0 },
      entered_battlefield_turn: null,
      ...overrides,
    });
  }

  it("is false for a card that isn't face down", () => {
    const obj = faceDownObject({ face_down: false, display_visible_to_viewer: true });
    expect(isFaceDownExileCardVisibleToViewer(buildGameState({ objects: {} }), obj, 1)).toBe(false);
  });

  it("uses only the engine-projected display bit", () => {
    const visible = faceDownObject({ display_visible_to_viewer: true });
    const hidden = faceDownObject({ display_visible_to_viewer: false, foretold: true });
    const state = buildGameState({ objects: buildObjectMap(visible, hidden) });

    expect(isFaceDownExileCardVisibleToViewer(state, visible, 1)).toBe(true);
    expect(isFaceDownExileCardVisibleToViewer(state, hidden, 0)).toBe(false);
  });
});

// ── The player axis of the click-target authority ────────────────────────────
//
// `getWaitingForClickTargetRefs` is the single WaitingFor -> engine-authored
// `TargetRef[]` authority, and `getWaitingForPlayerChoiceIds` is its player-axis
// projection. Every seat-rendering surface (PlayerHud, OpponentHud's 1v1 pill
// and multiplayer tabs, OpponentSeatHeader) reads the projection instead of
// hand-rolling a per-variant derivation.

/** Every fixture below mixes an object ref and a player ref so a `flatMap` that
 * forgot to narrow, or narrowed on the wrong key, cannot pass. */
const MIXED_LEGAL: TargetRef[] = [{ Object: 7 }, { Player: 1 }];

/**
 * Every `WaitingFor` variant, mapped to the fixture that pins how the two
 * click-target authorities treat it. This map is the drift gate the
 * two-switch design owes: `Record<WaitingFor["type"], …>` is total, so the
 * day `types.ts` gains a variant, `pnpm run type-check` fails here until
 * someone records what the player axis does with it. A variant added to
 * `getWaitingForObjectChoiceIds` alone can no longer land behind a green
 * suite.
 *
 * `NO_TARGET_REF_LEGAL_SET` records a checked claim: the variant carries no
 * `TargetRef[]` legal set at all, so neither authority can name a player for
 * it. Before writing it, apply the two-criteria test in
 * `getWaitingForClickTargetRefs`'s doc comment.
 *
 * The `NO_TARGET_REF_LEGAL_SET` entries are SKIPPED at runtime — they exist
 * only for the compile-time gate. The map's size is not a runtime coverage
 * number; the 11 `PartitionFixture` entries are.
 */
const NO_TARGET_REF_LEGAL_SET = "no-TargetRef-legal-set" as const;

interface PartitionFixture {
  waitingFor: WaitingFor;
  /** The engine-authored legal list the two authorities must partition. */
  legal: TargetRef[];
}

const PARTITION_FIXTURES: Record<
  WaitingFor["type"],
  PartitionFixture | typeof NO_TARGET_REF_LEGAL_SET
> = {
  // ── The 11 `TargetRef`-bearing variants ────────────────────────────────
  TargetSelection: {
    waitingFor: targetSelectionWaitingForFactory
      .withData({
        selection: buildTargetSelectionProgress({ current_legal_targets: MIXED_LEGAL }),
        target_slots: [buildTargetSelectionSlot({ legal_targets: MIXED_LEGAL })],
      })
      .forPlayer(0)
      .build(),
    legal: MIXED_LEGAL,
  },
  TriggerTargetSelection: {
    waitingFor: triggerTargetSelectionWaitingForFactory
      .withData({
        selection: buildTargetSelectionProgress({ current_legal_targets: MIXED_LEGAL }),
        target_slots: [buildTargetSelectionSlot({ legal_targets: MIXED_LEGAL })],
      })
      .forPlayer(0)
      .build(),
    legal: MIXED_LEGAL,
  },
  CopyRetarget: {
    waitingFor: copyRetargetWaitingForFactory
      .withData({
        current_slot: 0,
        target_slots: [buildCopyTargetSlot({ legal_alternatives: MIXED_LEGAL })],
      })
      .forPlayer(0)
      .build(),
    legal: MIXED_LEGAL,
  },
  // Only the `Single`-scope shape belongs here: a `Record` keyed on `type`
  // cannot hold two entries for one variant, so the `All`-scope pair is
  // asserted by its own `it` below.
  RetargetChoice: {
    waitingFor: retargetChoiceWaitingForFactory
      .withData({ scope: { type: "Single" }, legal_new_targets: MIXED_LEGAL })
      .forPlayer(0)
      .build(),
    legal: MIXED_LEGAL,
  },
  ReturnAsAuraTarget: {
    waitingFor: returnAsAuraTargetWaitingForFactory
      .withData({ legal_targets: MIXED_LEGAL })
      .forPlayer(0)
      .build(),
    legal: MIXED_LEGAL,
  },
  // ── Dialog-only by design: answered by a DIFFERENT GameAction ──────────
  // Answered by `DistributeAmong { distribution }` — a click carries no amount.
  DistributeAmong: {
    waitingFor: {
      type: "DistributeAmong",
      data: { player: 0, total: 3, targets: MIXED_LEGAL, unit: { type: "Damage" } },
    },
    legal: MIXED_LEGAL,
  },
  // Answered by `SelectTargets { targets }` — CR 701.34a chooses an any-size
  // subset of permanents and/or players, which a single click cannot express.
  ProliferateChoice: {
    waitingFor: { type: "ProliferateChoice", data: { player: 0, eligible: MIXED_LEGAL } },
    legal: MIXED_LEGAL,
  },
  // Shares ProliferateModal and the same `SelectTargets` subset dispatch.
  TimeTravelChoice: {
    waitingFor: {
      type: "TimeTravelChoice",
      data: { player: 0, eligible: MIXED_LEGAL, phase: "Remove" },
    },
    legal: MIXED_LEGAL,
  },
  // Shares ProliferateModal; `SelectTargets` subset.
  ChooseObjectsSelection: {
    waitingFor: { type: "ChooseObjectsSelection", data: { player: 0, eligible: MIXED_LEGAL } },
    legal: MIXED_LEGAL,
  },
  // Answered by an ORDERED `SelectTargets` (first pick is copied, second
  // scales) — a click carries no order.
  EachPlayerCopyChosenSelection: {
    waitingFor: {
      type: "EachPlayerCopyChosenSelection",
      data: {
        player: 0,
        eligible: MIXED_LEGAL,
        min: 1,
        max: 2,
        choose_filter: {},
        source_id: 1,
        source_controller: 0,
        remaining_players: [],
        all_choices: [],
        scoped_players: [0],
      },
    },
    legal: MIXED_LEGAL,
  },
  // Answered by `ChooseBranch { index }`; `parent_targets` is continuation
  // context the modal never reads.
  ChooseOneOfBranch: {
    waitingFor: {
      type: "ChooseOneOfBranch",
      data: { player: 0, controller: 0, source_id: 1, branches: [], parent_targets: MIXED_LEGAL },
    },
    legal: MIXED_LEGAL,
  },
  // ── Everything else carries no `TargetRef[]` legal set at all ──────────
  DeclareAttackers: NO_TARGET_REF_LEGAL_SET,
  DeclareBlockers: NO_TARGET_REF_LEGAL_SET,
  Priority: NO_TARGET_REF_LEGAL_SET,
  ResolveAllConsent: NO_TARGET_REF_LEGAL_SET,
  ResolveAllReady: NO_TARGET_REF_LEGAL_SET,
  MeldPairChoice: NO_TARGET_REF_LEGAL_SET,
  MeldAttackTargetChoice: NO_TARGET_REF_LEGAL_SET,
  EntryAttackTargetChoice: NO_TARGET_REF_LEGAL_SET,
  ActivationCostOneOfChoice: NO_TARGET_REF_LEGAL_SET,
  MulliganDecision: NO_TARGET_REF_LEGAL_SET,
  OpeningHandBottomCards: NO_TARGET_REF_LEGAL_SET,
  ManaPayment: NO_TARGET_REF_LEGAL_SET,
  ManaSourceSelection: NO_TARGET_REF_LEGAL_SET,
  ChooseXValue: NO_TARGET_REF_LEGAL_SET,
  PayAmountChoice: NO_TARGET_REF_LEGAL_SET,
  GameOver: NO_TARGET_REF_LEGAL_SET,
  ReplacementChoice: NO_TARGET_REF_LEGAL_SET,
  EntryControllerChoice: NO_TARGET_REF_LEGAL_SET,
  OrderTriggers: NO_TARGET_REF_LEGAL_SET,
  CopyTargetChoice: NO_TARGET_REF_LEGAL_SET,
  ExploreChoice: NO_TARGET_REF_LEGAL_SET,
  EquipTarget: NO_TARGET_REF_LEGAL_SET,
  CrewVehicle: NO_TARGET_REF_LEGAL_SET,
  StationTarget: NO_TARGET_REF_LEGAL_SET,
  SaddleMount: NO_TARGET_REF_LEGAL_SET,
  ScryChoice: NO_TARGET_REF_LEGAL_SET,
  ArrangePlanarDeckTopChoice: NO_TARGET_REF_LEGAL_SET,
  RedistributeLifeTotals: NO_TARGET_REF_LEGAL_SET,
  CoinFlipKeepChoice: NO_TARGET_REF_LEGAL_SET,
  DigChoice: NO_TARGET_REF_LEGAL_SET,
  SurveilChoice: NO_TARGET_REF_LEGAL_SET,
  RevealChoice: NO_TARGET_REF_LEGAL_SET,
  SearchChoice: NO_TARGET_REF_LEGAL_SET,
  SearchPartitionChoice: NO_TARGET_REF_LEGAL_SET,
  OutsideGameChoice: NO_TARGET_REF_LEGAL_SET,
  BetweenGamesSideboard: NO_TARGET_REF_LEGAL_SET,
  BetweenGamesChoosePlayDraw: NO_TARGET_REF_LEGAL_SET,
  NamedChoice: NO_TARGET_REF_LEGAL_SET,
  OpponentGuess: NO_TARGET_REF_LEGAL_SET,
  SpellbookDraft: NO_TARGET_REF_LEGAL_SET,
  DamageSourceChoice: NO_TARGET_REF_LEGAL_SET,
  ModeChoice: NO_TARGET_REF_LEGAL_SET,
  AbilityModeChoice: NO_TARGET_REF_LEGAL_SET,
  DiscardToHandSize: NO_TARGET_REF_LEGAL_SET,
  OptionalCostChoice: NO_TARGET_REF_LEGAL_SET,
  CostTypeChoice: NO_TARGET_REF_LEGAL_SET,
  SpliceOffer: NO_TARGET_REF_LEGAL_SET,
  DefilerPayment: NO_TARGET_REF_LEGAL_SET,
  CastOffer: NO_TARGET_REF_LEGAL_SET,
  ModalFaceChoice: NO_TARGET_REF_LEGAL_SET,
  AlternativeCastChoice: NO_TARGET_REF_LEGAL_SET,
  MutateMergeChoice: NO_TARGET_REF_LEGAL_SET,
  CipherEncodeChoice: NO_TARGET_REF_LEGAL_SET,
  CastingVariantChoice: NO_TARGET_REF_LEGAL_SET,
  ChoosePermanentTypeSlot: NO_TARGET_REF_LEGAL_SET,
  MultiTargetSelection: NO_TARGET_REF_LEGAL_SET,
  MiracleReveal: NO_TARGET_REF_LEGAL_SET,
  PayCost: NO_TARGET_REF_LEGAL_SET,
  BlightChoice: NO_TARGET_REF_LEGAL_SET,
  PayManaAbilityMana: NO_TARGET_REF_LEGAL_SET,
  ChooseManaColor: NO_TARGET_REF_LEGAL_SET,
  CollectEvidenceChoice: NO_TARGET_REF_LEGAL_SET,
  HarmonizeTapChoice: NO_TARGET_REF_LEGAL_SET,
  OptionalEffectChoice: NO_TARGET_REF_LEGAL_SET,
  PairChoice: NO_TARGET_REF_LEGAL_SET,
  OpponentMayChoice: NO_TARGET_REF_LEGAL_SET,
  LoopShortcut: NO_TARGET_REF_LEGAL_SET,
  RespondToShortcut: NO_TARGET_REF_LEGAL_SET,
  PrecastCopyShortcutOffer: NO_TARGET_REF_LEGAL_SET,
  RespondToPrecastCopyShortcut: NO_TARGET_REF_LEGAL_SET,
  UnlessPayment: NO_TARGET_REF_LEGAL_SET,
  UnlessPaymentChooseCost: NO_TARGET_REF_LEGAL_SET,
  WardDiscardChoice: NO_TARGET_REF_LEGAL_SET,
  WardSacrificeChoice: NO_TARGET_REF_LEGAL_SET,
  UnlessBounceChoice: NO_TARGET_REF_LEGAL_SET,
  ChooseRingBearer: NO_TARGET_REF_LEGAL_SET,
  RevealUntilKeptChoice: NO_TARGET_REF_LEGAL_SET,
  RepeatDecision: NO_TARGET_REF_LEGAL_SET,
  TopOrBottomChoice: NO_TARGET_REF_LEGAL_SET,
  PopulateChoice: NO_TARGET_REF_LEGAL_SET,
  CompanionReveal: NO_TARGET_REF_LEGAL_SET,
  ChooseLegend: NO_TARGET_REF_LEGAL_SET,
  CommanderZoneChoice: NO_TARGET_REF_LEGAL_SET,
  BattleProtectorChoice: NO_TARGET_REF_LEGAL_SET,
  TributeChoice: NO_TARGET_REF_LEGAL_SET,
  CombatTaxPayment: NO_TARGET_REF_LEGAL_SET,
  UntapChoice: NO_TARGET_REF_LEGAL_SET,
  ChooseUntapSubset: NO_TARGET_REF_LEGAL_SET,
  ExertChoice: NO_TARGET_REF_LEGAL_SET,
  EnlistChoice: NO_TARGET_REF_LEGAL_SET,
  PhyrexianPayment: NO_TARGET_REF_LEGAL_SET,
  AssignCombatDamage: NO_TARGET_REF_LEGAL_SET,
  AssignBlockerDamage: NO_TARGET_REF_LEGAL_SET,
  MoveCountersDistribution: NO_TARGET_REF_LEGAL_SET,
  RemoveCountersChoice: NO_TARGET_REF_LEGAL_SET,
  ChooseFromZoneChoice: NO_TARGET_REF_LEGAL_SET,
  BeholdChoice: NO_TARGET_REF_LEGAL_SET,
  EffectZoneChoice: NO_TARGET_REF_LEGAL_SET,
  DrawnThisTurnTopdeckChoice: NO_TARGET_REF_LEGAL_SET,
  AssistChoosePlayer: NO_TARGET_REF_LEGAL_SET,
  AssistPayment: NO_TARGET_REF_LEGAL_SET,
  ConniveDiscard: NO_TARGET_REF_LEGAL_SET,
  DiscardChoice: NO_TARGET_REF_LEGAL_SET,
  ManifestDreadChoice: NO_TARGET_REF_LEGAL_SET,
  LearnChoice: NO_TARGET_REF_LEGAL_SET,
  ClashChooseOpponent: NO_TARGET_REF_LEGAL_SET,
  ChooseFromZoneOpponentChooser: NO_TARGET_REF_LEGAL_SET,
  ChooseAnnouncingOpponent: NO_TARGET_REF_LEGAL_SET,
  ChooseGiftRecipient: NO_TARGET_REF_LEGAL_SET,
  ClashCardPlacement: NO_TARGET_REF_LEGAL_SET,
  VoteChoice: NO_TARGET_REF_LEGAL_SET,
  ChooseDungeon: NO_TARGET_REF_LEGAL_SET,
  ChooseDungeonRoom: NO_TARGET_REF_LEGAL_SET,
  SpecializeColor: NO_TARGET_REF_LEGAL_SET,
  ChooseRoomDoor: NO_TARGET_REF_LEGAL_SET,
  CategoryChoice: NO_TARGET_REF_LEGAL_SET,
  KeepWithinTotalPowerChoice: NO_TARGET_REF_LEGAL_SET,
  KeepExactPermanentsChoice: NO_TARGET_REF_LEGAL_SET,
  SeparatePilesChooseOpponent: NO_TARGET_REF_LEGAL_SET,
  SeparatePilesPartition: NO_TARGET_REF_LEGAL_SET,
  SeparatePilesChoice: NO_TARGET_REF_LEGAL_SET,
};

describe("getWaitingForPlayerChoiceIds", () => {
  // V1 — one `it` per `case` body, each on a MIXED list so the object half is
  // proven to be dropped rather than coincidentally absent.
  it("projects TargetSelection player refs and leaves the object refs to the object axis", () => {
    const wf = targetSelectionWaitingForFactory
      .withData({
        selection: buildTargetSelectionProgress({ current_legal_targets: MIXED_LEGAL }),
        target_slots: [buildTargetSelectionSlot({ legal_targets: MIXED_LEGAL })],
      })
      .forPlayer(0)
      .build();

    expect(getWaitingForPlayerChoiceIds(wf)).toEqual([1]);
    expect(getWaitingForObjectChoiceIds(wf)).toEqual([7]);
  });

  it("projects TriggerTargetSelection player refs", () => {
    const wf = triggerTargetSelectionWaitingForFactory
      .withData({
        selection: buildTargetSelectionProgress({ current_legal_targets: MIXED_LEGAL }),
        target_slots: [buildTargetSelectionSlot({ legal_targets: MIXED_LEGAL })],
      })
      .forPlayer(0)
      .build();

    expect(getWaitingForPlayerChoiceIds(wf)).toEqual([1]);
    expect(getWaitingForObjectChoiceIds(wf)).toEqual([7]);
  });

  // CR 707.10c: the copy's controller retargets one slot at a time.
  it("projects CopyRetarget player refs", () => {
    const wf = copyRetargetWaitingForFactory
      .withData({
        current_slot: 0,
        target_slots: [buildCopyTargetSlot({ legal_alternatives: MIXED_LEGAL })],
      })
      .forPlayer(0)
      .build();

    expect(getWaitingForPlayerChoiceIds(wf)).toEqual([1]);
    expect(getWaitingForObjectChoiceIds(wf)).toEqual([7]);
  });

  // CR 115.7: Bolt Bend / Misdirection retarget a single-target spell by a click.
  it("projects RetargetChoice(Single) player refs", () => {
    const wf = retargetChoiceWaitingForFactory
      .withData({ scope: { type: "Single" }, legal_new_targets: MIXED_LEGAL })
      .forPlayer(0)
      .build();

    expect(getWaitingForPlayerChoiceIds(wf)).toEqual([1]);
    expect(getWaitingForObjectChoiceIds(wf)).toEqual([7]);
  });

  // CR 303.4: an Aura enters attached to an object OR a player, so this legal
  // list genuinely mixes both axes in production.
  it("projects ReturnAsAuraTarget player refs", () => {
    const wf = returnAsAuraTargetWaitingForFactory
      .withData({ legal_targets: MIXED_LEGAL })
      .forPlayer(0)
      .build();

    expect(getWaitingForPlayerChoiceIds(wf)).toEqual([1]);
    expect(getWaitingForObjectChoiceIds(wf)).toEqual([7]);
  });

  it("returns [] for a null or absent waiting state", () => {
    expect(getWaitingForPlayerChoiceIds(null)).toEqual([]);
    expect(getWaitingForPlayerChoiceIds(undefined)).toEqual([]);
    expect(getWaitingForClickTargetRefs(null)).toBeNull();
  });

  // V4 — the slot axis. The two slots offer DIFFERENT seats, so reading slot 0
  // unconditionally is observable rather than coincidentally equal.
  it("reads CopyRetarget's current slot, not slot 0", () => {
    const wf = copyRetargetWaitingForFactory
      .withData({
        current_slot: 1,
        target_slots: [
          buildCopyTargetSlot({ legal_alternatives: [{ Player: 3 }] }),
          buildCopyTargetSlot({ legal_alternatives: [{ Player: 1 }] }),
        ],
      })
      .forPlayer(0)
      .build();

    expect(getWaitingForPlayerChoiceIds(wf)).toEqual([1]);
  });

  it("falls back to CopyRetarget slot 0 when current_slot is omitted", () => {
    const wf = copyRetargetWaitingForFactory
      .withData({
        target_slots: [
          buildCopyTargetSlot({ legal_alternatives: [{ Player: 3 }] }),
          buildCopyTargetSlot({ legal_alternatives: [{ Player: 1 }] }),
        ],
      })
      .forPlayer(0)
      .build();
    delete wf.data.current_slot;

    expect(getWaitingForPlayerChoiceIds(wf)).toEqual([3]);
  });

  // V3 — an `All`-scope retarget is not a click prompt at all: the engine has no
  // `ChooseTarget` apply arm for it, and RetargetChoiceModal keeps its pointer
  // events for the confirm button. The paired `Single` positive on the IDENTICAL
  // payload proves the exclusion keys on the scope, not on the whole variant.
  it("excludes RetargetChoice(All) while accepting the identical Single payload", () => {
    const allScope = retargetChoiceWaitingForFactory
      .withData({ scope: { type: "All" }, legal_new_targets: MIXED_LEGAL })
      .forPlayer(0)
      .build();
    const singleScope = retargetChoiceWaitingForFactory
      .withData({ scope: { type: "Single" }, legal_new_targets: MIXED_LEGAL })
      .forPlayer(0)
      .build();

    expect(getWaitingForClickTargetRefs(allScope)).toBeNull();
    expect(getWaitingForPlayerChoiceIds(allScope)).toEqual([]);
    expect(getWaitingForPlayerChoiceIds(singleScope)).toEqual([1]);
  });
});

// V5 — the six dialog-only variants. Each carries `{Player: 1}` in its own
// `TargetRef[]` field, and a `TargetSelection` built from the IDENTICAL list
// returns `[1]`, so the exclusion is proven to key on the variant rather than on
// the list contents.
describe("getWaitingForPlayerChoiceIds — dialog-only variants", () => {
  const DIALOG_ONLY = [
    "DistributeAmong",
    "ProliferateChoice",
    "TimeTravelChoice",
    "ChooseObjectsSelection",
    "EachPlayerCopyChosenSelection",
    "ChooseOneOfBranch",
  ] as const;

  it.each(DIALOG_ONLY)("excludes %s (answered by a different GameAction)", (variant) => {
    const fixture = PARTITION_FIXTURES[variant];
    if (fixture === NO_TARGET_REF_LEGAL_SET) throw new Error(`${variant} must have a fixture`);

    expect(fixture.legal).toContainEqual({ Player: 1 });
    expect(getWaitingForClickTargetRefs(fixture.waitingFor)).toBeNull();
    expect(getWaitingForPlayerChoiceIds(fixture.waitingFor)).toEqual([]);
  });

  it("returns [1] for a TargetSelection built from the identical legal list", () => {
    expect(
      getWaitingForPlayerChoiceIds(
        targetSelectionWaitingForFactory
          .withData({
            selection: buildTargetSelectionProgress({ current_legal_targets: MIXED_LEGAL }),
            target_slots: [buildTargetSelectionSlot({ legal_targets: MIXED_LEGAL })],
          })
          .forPlayer(0)
          .build(),
      ),
    ).toEqual([1]);
  });
});

// V2 — the partition rule, total over every fixture in the map. The compile-time
// half of this gate is the `Record<WaitingFor["type"], …>` key set itself.
describe("the two click-target authorities partition the engine's legal list", () => {
  const entries = Object.entries(PARTITION_FIXTURES).flatMap(([type, fixture]) =>
    fixture === NO_TARGET_REF_LEGAL_SET ? [] : [[type, fixture] as const],
  );

  it("covers every TargetRef-bearing variant", () => {
    expect(entries.map(([type]) => type).sort()).toEqual(
      [
        "ChooseObjectsSelection",
        "ChooseOneOfBranch",
        "CopyRetarget",
        "DistributeAmong",
        "EachPlayerCopyChosenSelection",
        "ProliferateChoice",
        "RetargetChoice",
        "ReturnAsAuraTarget",
        "TargetSelection",
        "TimeTravelChoice",
        "TriggerTargetSelection",
      ].sort(),
    );
  });

  it.each(entries)("partitions %s", (_type, fixture) => {
    // Every fixture must be non-degenerate: a legal list with only one axis
    // could not tell a correct partition from a broken one.
    expect(fixture.legal.some((ref) => "Object" in ref)).toBe(true);
    expect(fixture.legal.some((ref) => "Player" in ref)).toBe(true);

    const refs = getWaitingForClickTargetRefs(fixture.waitingFor);
    const objects = getWaitingForObjectChoiceIds(fixture.waitingFor);
    const players = getWaitingForPlayerChoiceIds(fixture.waitingFor);

    if (refs === null) {
      // Not a click prompt: neither axis may offer anything, even though the
      // engine's list is non-empty and names both an object and a player.
      expect(objects).toEqual([]);
      expect(players).toEqual([]);
      return;
    }

    expect(refs).toEqual(fixture.legal);
    expect(objects).toEqual(
      fixture.legal.flatMap((ref) => ("Object" in ref ? [ref.Object] : [])),
    );
    expect(players).toEqual(
      fixture.legal.flatMap((ref) => ("Player" in ref ? [ref.Player] : [])),
    );
  });
});
