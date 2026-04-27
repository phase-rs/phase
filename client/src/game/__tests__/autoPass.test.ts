import { describe, it, expect } from "vitest";

import type { GameState, Phase, WaitingFor } from "../../adapter/types";
import { shouldAutoPass } from "../autoPass";

/**
 * Creates a minimal GameState for auto-pass testing.
 * Only fields accessed by shouldAutoPass are populated.
 */
function createState(overrides: {
  phase?: Phase;
  stack?: unknown[];
  objects?: Record<string, unknown>;
  players?: unknown[];
  phase_stops?: Record<number, Phase[]>;
} = {}): GameState {
  return {
    phase: overrides.phase ?? "PreCombatMain",
    stack: overrides.stack ?? [],
    objects: overrides.objects ?? { 1: { id: 1 } },
    players: overrides.players ?? [{ id: 0 }, { id: 1 }],
    phase_stops: overrides.phase_stops,
  } as unknown as GameState;
}

function priority(player: number): WaitingFor {
  return { type: "Priority", data: { player } } as WaitingFor;
}

describe("shouldAutoPass", () => {
  it("auto-passes when engine recommends it", () => {
    expect(shouldAutoPass(createState(), priority(0), false, true)).toBe(true);
  });

  it("does not auto-pass when engine does not recommend it", () => {
    expect(shouldAutoPass(createState(), priority(0), false, false)).toBe(false);
  });

  it("does not auto-pass in full control mode even if engine recommends it", () => {
    expect(shouldAutoPass(createState(), priority(0), true, true)).toBe(false);
  });

  it("does not auto-pass for non-Priority waiting states", () => {
    const mulligan: WaitingFor = {
      type: "MulliganDecision",
      data: { player: 0, mulligan_count: 0, free_first_mulligan: false },
    } as WaitingFor;
    expect(shouldAutoPass(createState(), mulligan, false, true)).toBe(false);
  });

  it("does not auto-pass when it is not the local player's priority", () => {
    expect(shouldAutoPass(createState(), priority(1), false, true)).toBe(false);
  });

  // Phase stops — only apply to initial priority (empty stack)
  it("does not auto-pass during a stopped phase with empty stack", () => {
    const state = createState({
      phase: "PreCombatMain",
      phase_stops: { 0: ["PreCombatMain"] },
    });
    expect(shouldAutoPass(state, priority(0), false, true)).toBe(false);
  });

  it("auto-passes in phase without a stop even if other phases have stops", () => {
    const state = createState({
      phase: "PreCombatMain",
      phase_stops: { 0: ["BeginCombat"] },
    });
    expect(shouldAutoPass(state, priority(0), false, true)).toBe(true);
  });

  it("ignores phase stops when stack is non-empty (responding to spell)", () => {
    const stateWithStack = createState({
      phase: "PreCombatMain",
      stack: [{ id: 1, card_id: 5, controller: 0 }],
      phase_stops: { 0: ["PreCombatMain"] },
    });
    expect(shouldAutoPass(stateWithStack, priority(0), false, true)).toBe(true);
  });

  it("treats another player's phase stops as irrelevant to local auto-pass", () => {
    // Phase stops are per-player; player 1's stops must not gate player 0.
    const state = createState({
      phase: "PreCombatMain",
      phase_stops: { 1: ["PreCombatMain"] },
    });
    expect(shouldAutoPass(state, priority(0), false, true)).toBe(true);
  });

  it("does not auto-pass with no objects in game state (invalid state)", () => {
    const emptyState = createState({ objects: {} });
    expect(shouldAutoPass(emptyState, priority(0), false, true)).toBe(false);
  });

  it("does not auto-pass with no players in game state (invalid state)", () => {
    const state = createState();
    (state as unknown as { players: unknown[] }).players = [];
    expect(shouldAutoPass(state, priority(0), false, true)).toBe(false);
  });
});
