import { describe, expect, it } from "vitest";

import type { ExileLinkKind, GameAction, GameObject, GameState, PlayerId, WaitingFor } from "../../adapter/types";
import {
  buildGameObject,
  buildGameObjectWithCoreTypes,
  buildObjectMap,
} from "../../test/factories/gameObjectFactory";
import {
  buildGameState,
  buildGameStateWithoutSeatOrder,
  buildPlayers,
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
  getWaitingForObjectChoiceIds,
  isFaceDownExileCardVisibleToViewer,
  isOneOnOne,
  isSplitBoardActive,
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

// Issue #2889: single-player renders the raw, unredacted state, so a
// Hideaway/Foretell face-down exile's real `name`/`printed_ref` sit on the
// object regardless of viewer. This helper is the client-side half of the
// engine's `hidden_facedown_exile_ids` look-permission gate
// (crates/engine/src/game/visibility.rs, CR 406.3 + CR 702.75a + CR 702.143e).
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

  function stateWithSourceAndExiled(
    source: GameObject,
    exiled: GameObject,
    kind: ExileLinkKind,
  ): GameState {
    return buildGameState({
      objects: buildObjectMap(source, exiled),
      exile_links: [{ exiled_id: exiled.id, source_id: source.id, kind }],
    });
  }

  it("is false for a card that isn't face down", () => {
    const obj = faceDownObject({ face_down: false });
    expect(isFaceDownExileCardVisibleToViewer(buildGameState({ objects: {} }), obj, 1)).toBe(false);
  });

  it("is true for the controller of the Hideaway permanent that exiled it", () => {
    const source: GameObject = { ...faceDownObject(), id: 1, zone: "Battlefield", face_down: false };
    const exiled = faceDownObject();
    const state = stateWithSourceAndExiled(source, exiled, "HideawayLookable");
    expect(isFaceDownExileCardVisibleToViewer(state, exiled, 1)).toBe(true);
  });

  it("is false for an opponent of the Hideaway permanent's controller", () => {
    const source: GameObject = { ...faceDownObject(), id: 1, zone: "Battlefield", face_down: false };
    const exiled = faceDownObject();
    const state = stateWithSourceAndExiled(source, exiled, "HideawayLookable");
    expect(isFaceDownExileCardVisibleToViewer(state, exiled, 0)).toBe(false);
  });

  it("is false for a plain TrackedBySource link even for the source's controller", () => {
    // Bomat Courier ("(You can't look at it.)") tracks its face-down exile by
    // source for later retrieval but grants no look-permission.
    const source: GameObject = { ...faceDownObject(), id: 1, zone: "Battlefield", face_down: false };
    const exiled = faceDownObject();
    const state = stateWithSourceAndExiled(source, exiled, "TrackedBySource");
    expect(isFaceDownExileCardVisibleToViewer(state, exiled, 1)).toBe(false);
  });

  it("is true for the owner of a foretold card", () => {
    const exiled = faceDownObject({ owner: 0, controller: 0, foretold: true });
    const state = buildGameState({ objects: buildObjectMap(exiled), exile_links: [] });
    expect(isFaceDownExileCardVisibleToViewer(state, exiled, 0)).toBe(true);
  });

  it("is false for an opponent of a foretold card's owner", () => {
    const exiled = faceDownObject({ owner: 0, controller: 0, foretold: true });
    const state = buildGameState({ objects: buildObjectMap(exiled), exile_links: [] });
    expect(isFaceDownExileCardVisibleToViewer(state, exiled, 1)).toBe(false);
  });
});
