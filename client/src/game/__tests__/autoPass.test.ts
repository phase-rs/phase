import { describe, it, expect } from "vitest";

import type { GameAction, GameState, Phase, WaitingFor } from "../../adapter/types";
import { shouldAutoPass } from "../autoPass";

/**
 * Creates a minimal GameState for auto-pass testing.
 * Only fields accessed by shouldAutoPass are populated.
 */
function createState(overrides: {
  phase?: Phase;
  stack?: unknown[];
  battlefield?: number[];
  objects?: Record<string, unknown>;
  turn_number?: number;
} = {}): GameState {
  return {
    phase: overrides.phase ?? "PreCombatMain",
    stack: overrides.stack ?? [],
    battlefield: overrides.battlefield ?? [],
    objects: overrides.objects ?? {},
    turn_number: overrides.turn_number ?? 1,
  } as unknown as GameState;
}

function createCreature(
  id: number,
  controller: number,
  opts: { tapped?: boolean; enteredTurn?: number; keywords?: string[] } = {},
) {
  return {
    [id]: {
      id,
      controller,
      card_types: { core_types: ["Creature"] },
      tapped: opts.tapped ?? false,
      entered_battlefield_turn: opts.enteredTurn ?? 0,
      keywords: opts.keywords ?? [],
    },
  };
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
      stack: [{ id: 1, card_id: 5, controller: 0 }],
    });
    expect(
      shouldAutoPass(stateWithStack, priority(0), stops, false, PASS_ONLY),
    ).toBe(true);
  });

  // Key scenario: creature on stack, no instant-speed responses
  it("auto-passes with items on the stack when player has no responses", () => {
    const stateWithStack = createState({
      stack: [{ id: 1, card_id: 5, controller: 0 }],
    });
    expect(
      shouldAutoPass(stateWithStack, priority(0), [], false, PASS_ONLY),
    ).toBe(true);
  });

  // MTGA-style: auto-pass when our own spell is on top of the stack
  it("auto-passes when own spell is on top of stack even with instant responses", () => {
    const stateWithStack = createState({
      stack: [{ id: 1, card_id: 5, controller: 0 }],
    });
    expect(
      shouldAutoPass(stateWithStack, priority(0), [], false, HAS_INSTANT),
    ).toBe(true);
  });

  // Must NOT auto-pass when opponent's spell is on top — player may want to counter
  it("does not auto-pass when opponent spell is on top and player has instant", () => {
    const stateWithStack = createState({
      stack: [{ id: 1, card_id: 5, controller: 1 }],
    });
    expect(
      shouldAutoPass(stateWithStack, priority(0), [], false, HAS_INSTANT),
    ).toBe(false);
  });

  it("auto-passes when opponent spell is on top but player has no responses", () => {
    const stateWithStack = createState({
      stack: [{ id: 1, card_id: 5, controller: 1 }],
    });
    expect(
      shouldAutoPass(stateWithStack, priority(0), [], false, PASS_ONLY),
    ).toBe(true);
  });

  it("auto-passes with empty legal actions array", () => {
    expect(
      shouldAutoPass(createState(), priority(0), [], false, []),
    ).toBe(true);
  });

  // Engine handles combat gating via has_potential_attackers at BeginCombat,
  // so auto-pass always passes when only PassPriority is available.
  it("auto-passes PreCombatMain even with eligible attackers when only pass available", () => {
    const state = createState({
      phase: "PreCombatMain",
      turn_number: 2,
      battlefield: [10],
      objects: createCreature(10, 0, { enteredTurn: 1 }),
    });
    expect(shouldAutoPass(state, priority(0), [], false, PASS_ONLY)).toBe(true);
  });
});
