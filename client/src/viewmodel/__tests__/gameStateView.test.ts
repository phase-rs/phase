import { describe, expect, it } from "vitest";

import type { GameState, PlayerId } from "../../adapter/types";
import {
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
