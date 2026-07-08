import { describe, it, expect } from "vitest";

import type { GameState, Phase, WaitingFor } from "../../adapter/types";
import { buildGameObject, buildObjectMap } from "../../test/factories/gameObjectFactory";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
} from "../../test/factories/gameStateFactory";
import { shouldAutoPass } from "../autoPass";

/**
 * Creates a minimal GameState for auto-pass testing.
 * Only fields accessed by shouldAutoPass are populated.
 */
function createState(overrides: {
  phase?: Phase;
  priority_player?: number;
  stack?: GameState["stack"];
  objects?: GameState["objects"];
  players?: GameState["players"];
} = {}): GameState {
  return buildGameState({
    phase: overrides.phase ?? "PreCombatMain",
    stack: overrides.stack ?? [],
    objects: overrides.objects ?? buildObjectMap(buildGameObject({ id: 1 })),
    players: overrides.players ?? buildPlayers([0, 1]),
    priority_player: overrides.priority_player ?? 0,
  });
}

function priority(player: number): WaitingFor {
  return buildPriorityWaitingFor({ data: { player } });
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
      data: {
        pending: [{ player: 0, mulligan_count: 0, phase: { type: "Declare" } }],
        free_first_mulligan: false,
      },
    };
    expect(shouldAutoPass(createState(), mulligan, false, true)).toBe(false);
  });

  it("does not auto-pass when it is not the local player's priority", () => {
    expect(shouldAutoPass(createState({ priority_player: 1 }), priority(1), false, true)).toBe(
      false,
    );
  });

  it("auto-passes when the local player controls another player's turn", () => {
    expect(shouldAutoPass(createState({ priority_player: 0 }), priority(1), false, true)).toBe(
      true,
    );
  });

  it("does not auto-pass when another player controls the local player's turn", () => {
    expect(shouldAutoPass(createState({ priority_player: 1 }), priority(0), false, true)).toBe(
      false,
    );
  });

  it("does not auto-pass with no objects in game state (invalid state)", () => {
    const emptyState = createState({ objects: {} });
    expect(shouldAutoPass(emptyState, priority(0), false, true)).toBe(false);
  });

  it("does not auto-pass with no players in game state (invalid state)", () => {
    const state = createState({ players: [] });
    expect(shouldAutoPass(state, priority(0), false, true)).toBe(false);
  });
});
