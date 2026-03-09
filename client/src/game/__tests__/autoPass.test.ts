import { describe, it, expect } from "vitest";

import type { GameAction, GameState, Phase, WaitingFor } from "../../adapter/types";
import { shouldAutoPass } from "../autoPass";

/**
 * Creates a minimal GameState for auto-pass testing.
 * Only fields accessed by shouldAutoPass are populated.
 */
function createState(overrides: { phase?: Phase; stack?: unknown[] } = {}): GameState {
  return {
    phase: overrides.phase ?? "PreCombatMain",
    stack: overrides.stack ?? [],
  } as unknown as GameState;
}

function priority(player: number): WaitingFor {
  return { type: "Priority", data: { player } } as WaitingFor;
}

const PASS_ONLY: GameAction[] = [{ type: "PassPriority" }];

const HAS_INSTANT: GameAction[] = [
  { type: "PassPriority" },
  { type: "CastSpell", data: { card_id: 1, targets: [] } },
];

const HAS_LAND: GameAction[] = [
  { type: "PassPriority" },
  { type: "PlayLand", data: { card_id: 2 } },
];

const HAS_ABILITY: GameAction[] = [
  { type: "PassPriority" },
  { type: "ActivateAbility", data: { source_id: 3, ability_index: 0 } },
];

describe("shouldAutoPass", () => {
  it("auto-passes when only PassPriority is available", () => {
    expect(
      shouldAutoPass(createState(), priority(0), [], false, PASS_ONLY),
    ).toBe(true);
  });

  it("does not auto-pass when a spell can be cast", () => {
    expect(
      shouldAutoPass(createState(), priority(0), [], false, HAS_INSTANT),
    ).toBe(false);
  });

  it("does not auto-pass when a land can be played", () => {
    expect(
      shouldAutoPass(createState(), priority(0), [], false, HAS_LAND),
    ).toBe(false);
  });

  it("does not auto-pass when an ability can be activated", () => {
    expect(
      shouldAutoPass(createState(), priority(0), [], false, HAS_ABILITY),
    ).toBe(false);
  });

  it("does not auto-pass in full control mode", () => {
    expect(
      shouldAutoPass(createState(), priority(0), [], true, PASS_ONLY),
    ).toBe(false);
  });

  it("does not auto-pass for non-Priority waiting states", () => {
    const mulligan: WaitingFor = {
      type: "MulliganDecision",
      data: { player: 0, mulligan_count: 0 },
    } as WaitingFor;
    expect(
      shouldAutoPass(createState(), mulligan, [], false, PASS_ONLY),
    ).toBe(false);
  });

  it("does not auto-pass when it is not the local player's priority", () => {
    expect(
      shouldAutoPass(createState(), priority(1), [], false, PASS_ONLY),
    ).toBe(false);
  });

  // Phase stops — only apply to initial priority (empty stack)
  it("does not auto-pass during a stopped phase with empty stack", () => {
    const stops: Phase[] = ["PreCombatMain"];
    expect(
      shouldAutoPass(createState({ phase: "PreCombatMain" }), priority(0), stops, false, PASS_ONLY),
    ).toBe(false);
  });

  it("auto-passes in phase without a stop even if other phases have stops", () => {
    const stops: Phase[] = ["BeginCombat"];
    expect(
      shouldAutoPass(createState({ phase: "PreCombatMain" }), priority(0), stops, false, PASS_ONLY),
    ).toBe(true);
  });

  it("ignores phase stops when stack is non-empty (responding to spell)", () => {
    const stops: Phase[] = ["PreCombatMain"];
    const stateWithStack = createState({
      phase: "PreCombatMain",
      stack: [{ id: 1, card_id: 5 }],
    });
    expect(
      shouldAutoPass(stateWithStack, priority(0), stops, false, PASS_ONLY),
    ).toBe(true);
  });

  // Key scenario: creature on stack, no instant-speed responses
  it("auto-passes with items on the stack when player has no responses", () => {
    const stateWithStack = createState({
      stack: [{ id: 1, card_id: 5 }],
    });
    expect(
      shouldAutoPass(stateWithStack, priority(0), [], false, PASS_ONLY),
    ).toBe(true);
  });

  // Key scenario: creature on stack, player has an instant to respond with
  it("does not auto-pass with items on stack when player has instant response", () => {
    const stateWithStack = createState({
      stack: [{ id: 1, card_id: 5 }],
    });
    expect(
      shouldAutoPass(stateWithStack, priority(0), [], false, HAS_INSTANT),
    ).toBe(false);
  });

  it("auto-passes with empty legal actions array", () => {
    expect(
      shouldAutoPass(createState(), priority(0), [], false, []),
    ).toBe(true);
  });
});
