import { describe, expect, it } from "vitest";

import type { GameAction, GameObject, GameState, PlayerId } from "../../adapter/types";
import {
  getCastableZoneViewerTarget,
  getOpponentIds,
  getSeatCount,
  getWaitingForObjectChoiceIds,
  isOneOnOne,
} from "../gameStateView";

// Test fixtures only populate the fields these helpers actually read.
// Cast through `unknown` so we don't have to hand-construct the full
// hundreds-of-fields GameState surface.
function makeState(seatOrder: PlayerId[], eliminated: PlayerId[] = []): GameState {
  return {
    seat_order: seatOrder,
    eliminated_players: eliminated,
    players: seatOrder.map((id) => ({ id })),
  } as unknown as GameState;
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
    const state = { players: [{ id: 0 }, { id: 1 }, { id: 2 }] } as unknown as GameState;
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

describe("getWaitingForObjectChoiceIds", () => {
  it("returns valid_tokens for PopulateChoice", () => {
    expect(
      getWaitingForObjectChoiceIds({
        type: "PopulateChoice",
        data: { player: 0, source_id: 1, valid_tokens: [10, 11] },
      }),
    ).toEqual([10, 11]);
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
    return {
      id,
      card_id: 700 + id,
      owner: 0,
      controller: 0,
      zone: "Graveyard",
      tapped: false,
      face_down: false,
      flipped: false,
      transformed: false,
      damage_marked: 0,
      dealt_deathtouch_damage: false,
      attached_to: null,
      attachments: [],
      counters: {},
      name: `Spell ${id}`,
      power: null,
      toughness: null,
      loyalty: null,
      card_types: { supertypes: [], core_types: ["Instant"], subtypes: [] },
      mana_cost: { type: "Cost", shards: ["Red"], generic: 0 },
      keywords: ["Retrace"],
      abilities: [],
      trigger_definitions: [],
      replacement_definitions: [],
      static_definitions: [],
      color: ["Red"],
      base_power: null,
      base_toughness: null,
      base_keywords: ["Retrace"],
      base_color: ["Red"],
      timestamp: 1,
      entered_battlefield_turn: null,
    } as GameObject;
  }

  it("returns the graveyard pile when Priority surfaces cast actions there", () => {
    const objects = {
      7: makeGraveyardObject(7),
      8: makeGraveyardObject(8),
    };
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
