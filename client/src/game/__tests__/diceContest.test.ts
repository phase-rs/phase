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
  it("builds a startingPlayer die payload using the engine's winner", () => {
    flashStartingPlayerContest([die(0, 20, 17), die(1, 20, 9), gameStarted], 0);
    const d = useUiStore.getState().diceRoll;
    expect(d).toMatchObject({ kind: "die", sides: 20, context: "startingPlayer", winner: 0 });
    expect(d?.kind === "die" && d.rolls).toEqual([
      { playerId: 0, value: 17 },
      { playerId: 1, value: 9 },
    ]);
  });

  it("shows each player's FINAL roll after tie rerolls", () => {
    // Round 1 ties at 11; round 2 decides. The overlay should show the decisive
    // values (18 vs 4), not the tied first round.
    flashStartingPlayerContest(
      [die(0, 20, 11), die(1, 20, 11), die(0, 20, 18), die(1, 20, 4), gameStarted],
      0,
    );
    const d = useUiStore.getState().diceRoll;
    expect(d?.kind === "die" && d.rolls).toEqual([
      { playerId: 0, value: 18 },
      { playerId: 1, value: 4 },
    ]);
  });

  it("honors the engine winner even when it isn't the highest shown roll (lowest-seat fallback)", () => {
    // Engine's all-tied-at-cap fallback picks the lowest seat; the winner is
    // passed in, never recomputed from the rolls.
    flashStartingPlayerContest([die(0, 20, 7), die(1, 20, 7), gameStarted], 0);
    expect(useUiStore.getState().diceRoll).toMatchObject({ winner: 0 });
  });

  it("no-ops when the starter was chosen explicitly (no leading DieRolled)", () => {
    flashStartingPlayerContest([gameStarted], 1);
    expect(useUiStore.getState().diceRoll).toBeNull();
  });

  it("respects instant animation speed (0): no overlay", () => {
    usePreferencesStore.setState({ animationSpeedMultiplier: 0 });
    flashStartingPlayerContest([die(0, 20, 5)], 0);
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
