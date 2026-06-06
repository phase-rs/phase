import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameEvent } from "../../adapter/types";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useUiStore } from "../../stores/uiStore";
import { flashInGameRolls, flashStartingPlayerContest } from "../diceContest";

const die = (player_id: number, sides: number, result: number): GameEvent => ({
  type: "DieRolled",
  data: { player_id, sides, result },
});
const coin = (player_id: number, won: boolean): GameEvent => ({
  type: "CoinFlipped",
  data: { player_id, won },
});
const gameStarted: GameEvent = { type: "GameStarted" };
// CR 103.1 starting-player contest: each round is a list of [playerId, value]
// rolls; round 0 is every seat, later rounds are the tied-max reroll group.
const contest = (rounds: [number, number][][], winner: number): GameEvent => ({
  type: "StartingPlayerContest",
  data: { rounds: rounds.map((rolls) => ({ rolls })), winner },
});

beforeEach(() => {
  vi.useFakeTimers();
  usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
  useUiStore.setState({ diceRoll: null, diceRollQueue: [] });
});

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe("flashStartingPlayerContest", () => {
  it("builds a startingPlayer die payload from the engine contest event", () => {
    flashStartingPlayerContest([contest([[[0, 17], [1, 9]]], 0), gameStarted], 0);
    const d = useUiStore.getState().diceRoll;
    expect(d).toMatchObject({ kind: "die", sides: 20, context: "startingPlayer", winner: 0 });
    expect(d?.kind === "die" && d.rounds).toEqual([
      [
        { playerId: 0, value: 17 },
        { playerId: 1, value: 9 },
      ],
    ]);
    // `rolls` mirrors the decisive (final) round.
    expect(d?.kind === "die" && d.rolls).toEqual([
      { playerId: 0, value: 17 },
      { playerId: 1, value: 9 },
    ]);
  });

  it("preserves the per-round structure across a tie reroll", () => {
    // Round 1 ties at 11; round 2 decides (18 vs 4). Each round is kept separate
    // so within the decisive round the winner (18) is the visible high roller —
    // no cross-round mixing (the bug this fixes).
    flashStartingPlayerContest(
      [
        contest(
          [
            [
              [0, 11],
              [1, 11],
            ],
            [
              [0, 18],
              [1, 4],
            ],
          ],
          0,
        ),
        gameStarted,
      ],
      0,
    );
    const d = useUiStore.getState().diceRoll;
    expect(d?.kind === "die" && d.rounds).toEqual([
      [
        { playerId: 0, value: 11 },
        { playerId: 1, value: 11 },
      ],
      [
        { playerId: 0, value: 18 },
        { playerId: 1, value: 4 },
      ],
    ]);
    expect(d?.kind === "die" && d.rolls).toEqual([
      { playerId: 0, value: 18 },
      { playerId: 1, value: 4 },
    ]);
  });

  it("uses the engine winner, never recomputed from the rolls (lowest-seat fallback)", () => {
    // The engine's all-tied-at-cap fallback picks the lowest seat; the winner is
    // passed in, not derived from the shown dice.
    flashStartingPlayerContest([contest([[[0, 7], [1, 7]]], 0), gameStarted], 0);
    expect(useUiStore.getState().diceRoll).toMatchObject({ winner: 0 });
  });

  it("no-ops when the starter was chosen explicitly (no contest event)", () => {
    flashStartingPlayerContest([gameStarted], 1);
    expect(useUiStore.getState().diceRoll).toBeNull();
  });

  it("skips the overlay entirely at instant animation speed (0)", () => {
    usePreferencesStore.setState({ animationSpeedMultiplier: 0 });
    flashStartingPlayerContest([contest([[[0, 5], [1, 3]]], 0), gameStarted], 0);
    expect(useUiStore.getState().diceRoll).toBeNull();
  });
});

describe("flashInGameRolls", () => {
  it("groups consecutive dice into one ability payload (e.g. Krark's Thumb double)", () => {
    flashInGameRolls([die(0, 6, 3), die(0, 6, 5)]);
    const d = useUiStore.getState().diceRoll;
    expect(d).toMatchObject({ kind: "die", sides: 6, context: "ability" });
    expect(d?.kind === "die" && d.rolls.length).toBe(2);
  });

  it("shows a coin flip when the batch has no dice", () => {
    flashInGameRolls([coin(1, true)]);
    expect(useUiStore.getState().diceRoll).toMatchObject({
      kind: "coin",
      playerId: 1,
      won: true,
      context: "ability",
    });
  });

  it("no-ops on a batch containing neither dice nor coins", () => {
    flashInGameRolls([gameStarted]);
    expect(useUiStore.getState().diceRoll).toBeNull();
  });

  it("queues a co-occurring coin behind the dice instead of dropping it", () => {
    flashInGameRolls([die(0, 20, 12), coin(0, true)]);
    const s = useUiStore.getState();
    expect(s.diceRoll).toMatchObject({ kind: "die" });
    expect(s.diceRollQueue).toEqual([
      { kind: "coin", playerId: 0, won: true, context: "ability" },
    ]);
  });

  it("plays queued rolls serially: dice → coin → idle", () => {
    flashInGameRolls([die(0, 20, 12), coin(0, true)]);
    expect(useUiStore.getState().diceRoll?.kind).toBe("die");
    vi.advanceTimersByTime(2400); // one DICE_ROLL_DURATION_MS at speed 1
    expect(useUiStore.getState().diceRoll).toMatchObject({ kind: "coin" });
    vi.advanceTimersByTime(2400);
    expect(useUiStore.getState().diceRoll).toBeNull();
  });
});
