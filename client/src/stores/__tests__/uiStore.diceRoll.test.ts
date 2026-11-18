import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  DICE_RESULT_HOLD_MS,
  DICE_RESULT_MIN_HOLD_MS,
  DICE_ROLL_LANDING_TIMEOUT_MS,
  DICE_ROLL_MIN_DURATION_MS,
} from "../../animation/types";
import { usePreferencesStore } from "../preferencesStore";
import { useUiStore } from "../uiStore";
import type { DiceRollPayload } from "../uiStore";

/** A fresh payload per call: the store tells rolls apart by identity. */
function abilityCoin(): DiceRollPayload {
  return { kind: "coin", playerId: 0, won: true, context: "ability" };
}

function contestRoll(): DiceRollPayload {
  return {
    kind: "die",
    sides: 20,
    context: "startingPlayer",
    winner: 0,
    rolls: [{ playerId: 0, value: 17 }, { playerId: 1, value: 4 }],
    rounds: [[{ playerId: 0, value: 17 }, { playerId: 1, value: 4 }]],
  };
}

function activeRoll(): DiceRollPayload | null {
  return useUiStore.getState().diceRoll;
}

function stalled(): boolean {
  return useUiStore.getState().diceRollStalled;
}

beforeEach(() => {
  vi.useFakeTimers();
  usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
});

afterEach(() => {
  useUiStore.getState().resetDiceRoll();
  usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe("settleDiceRoll: an ability roll's close countdown starts when it lands", () => {
  it.each([
    { speed: 2, holdMs: DICE_RESULT_HOLD_MS * 2 },
    { speed: 1, holdMs: DICE_RESULT_HOLD_MS },
    // #8992: the scaled beat would be a few hundred ms here, so the floor holds it.
    { speed: 0.25, holdMs: DICE_RESULT_MIN_HOLD_MS },
  ])("stays up until a late landing, then $holdMs ms more at speed $speed", ({ speed, holdMs }) => {
    usePreferencesStore.setState({ animationSpeedMultiplier: speed });
    const coin = abilityCoin();
    useUiStore.getState().flashDiceRoll(coin);

    // A slow chunk load or WebGL start-up lands the coin after the time a
    // countdown from the show would have allowed.
    vi.advanceTimersByTime(DICE_ROLL_MIN_DURATION_MS * speed + 500);
    expect(activeRoll()).toBe(coin);

    useUiStore.getState().settleDiceRoll(coin);
    vi.advanceTimersByTime(holdMs - 1);
    expect(activeRoll()).toBe(coin);
    vi.advanceTimersByTime(1);
    expect(activeRoll()).toBeNull();
  });

  it.each([
    { speed: 2, closeMs: DICE_ROLL_MIN_DURATION_MS * 2 },
    { speed: 1, closeMs: DICE_ROLL_MIN_DURATION_MS },
    // The read beat's floor outlasts the minimum here.
    { speed: 0.25, closeMs: DICE_RESULT_MIN_HOLD_MS },
  ])("keeps a roll that lands as it shows up $closeMs ms at speed $speed", ({ speed, closeMs }) => {
    usePreferencesStore.setState({ animationSpeedMultiplier: speed });
    const coin = abilityCoin();
    // A static face (reduced motion, no WebGL) reports its landing on mount.
    useUiStore.getState().flashDiceRoll(coin);
    useUiStore.getState().settleDiceRoll(coin);

    vi.advanceTimersByTime(closeMs - 1);
    expect(activeRoll()).toBe(coin);
    vi.advanceTimersByTime(1);
    expect(activeRoll()).toBeNull();
  });

  it("shows the static result when the landing never arrives, then counts down", () => {
    const coin = abilityCoin();
    const next = abilityCoin();
    useUiStore.getState().flashDiceRoll(coin);
    useUiStore.getState().flashDiceRoll(next);

    vi.advanceTimersByTime(DICE_ROLL_LANDING_TIMEOUT_MS - 1);
    expect(activeRoll()).toBe(coin);
    expect(stalled()).toBe(false);
    vi.advanceTimersByTime(1);
    expect(activeRoll()).toBe(coin);
    expect(stalled()).toBe(true);

    // The renderer landing after the fallback does not restart the countdown.
    useUiStore.getState().settleDiceRoll(coin);
    vi.advanceTimersByTime(DICE_RESULT_HOLD_MS - 1);
    expect(activeRoll()).toBe(coin);
    vi.advanceTimersByTime(1);
    expect(activeRoll()).toBe(next);
    expect(stalled()).toBe(false);
  });

  it("counts only the first landing report, so a repeat cannot push the close back", () => {
    const coin = abilityCoin();
    useUiStore.getState().flashDiceRoll(coin);
    vi.advanceTimersByTime(DICE_ROLL_MIN_DURATION_MS);
    useUiStore.getState().settleDiceRoll(coin);

    vi.advanceTimersByTime(DICE_RESULT_HOLD_MS - 1);
    useUiStore.getState().settleDiceRoll(coin);
    vi.advanceTimersByTime(1);
    expect(activeRoll()).toBeNull();
  });

  it("ignores a landing reported by a roll that is no longer showing", () => {
    const skipped = abilityCoin();
    const next = abilityCoin();
    useUiStore.getState().flashDiceRoll(skipped);
    useUiStore.getState().flashDiceRoll(next);
    useUiStore.getState().skipDiceRoll();
    expect(activeRoll()).toBe(next);

    // The skipped coin's renderer lands on its way out; the next roll keeps
    // waiting for its own landing, well past the time a countdown would take.
    useUiStore.getState().settleDiceRoll(skipped);
    vi.advanceTimersByTime(DICE_ROLL_MIN_DURATION_MS + DICE_RESULT_HOLD_MS);
    expect(activeRoll()).toBe(next);

    useUiStore.getState().settleDiceRoll(next);
    vi.advanceTimersByTime(DICE_RESULT_HOLD_MS);
    expect(activeRoll()).toBeNull();
  });

  it("closes at once after the landing when the speed was switched to instant", () => {
    const coin = abilityCoin();
    useUiStore.getState().flashDiceRoll(coin);
    usePreferencesStore.setState({ animationSpeedMultiplier: 0 });

    useUiStore.getState().settleDiceRoll(coin);
    vi.advanceTimersByTime(1);
    expect(activeRoll()).toBeNull();
  });

  it("holds the starting-player contest for the player after it lands", () => {
    const contest = contestRoll();
    useUiStore.getState().flashDiceRoll(contest);

    useUiStore.getState().settleDiceRoll(contest);
    vi.advanceTimersByTime(DICE_ROLL_LANDING_TIMEOUT_MS + DICE_RESULT_HOLD_MS);
    expect(activeRoll()).toBe(contest);
    expect(stalled()).toBe(false);
  });
});
