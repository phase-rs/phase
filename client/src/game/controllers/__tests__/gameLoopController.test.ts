import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, WaitingFor } from "../../../adapter/types";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory";
import { buildGameState, buildPlayers, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory";

const dispatchAction = vi.fn();
const dispatchResolveAll = vi.fn();

vi.mock("../../dispatch", () => ({
  dispatchAction: (action: unknown) => dispatchAction(action),
  dispatchResolveAll: (...args: unknown[]) => dispatchResolveAll(...args),
}));

vi.mock("../../../hooks/usePlayerId", () => ({
  getPlayerId: () => 0,
}));

let storeState: {
  waitingFor: WaitingFor | null;
  gameState: GameState | null;
  autoPassRecommended: boolean;
};
let waitingForSubscriber: (() => void) | null = null;

vi.mock("../../../stores/gameStore", () => ({
  useGameStore: {
    getState: () => storeState,
    setState: vi.fn(),
    subscribe: (_selector: unknown, callback: () => void) => {
      waitingForSubscriber = callback;
      return () => {
        if (waitingForSubscriber === callback) waitingForSubscriber = null;
      };
    },
  },
}));

let animationSpeedMultiplier = 1.0;

vi.mock("../../../stores/preferencesStore", () => ({
  usePreferencesStore: {
    getState: () => ({ aiSeats: [], animationSpeedMultiplier }),
  },
}));

vi.mock("../../../stores/uiStore", () => ({
  useUiStore: {
    getState: () => ({ fullControl: false }),
  },
}));

import { createGameLoopController } from "../gameLoopController";

function priority(player: number): WaitingFor {
  return buildPriorityWaitingFor({ data: { player } });
}

function stateFor(waitingFor: WaitingFor, priorityPlayer: number): GameState {
  return buildGameState({
    waiting_for: waitingFor,
    priority_player: priorityPlayer,
    phase: "PreCombatMain",
    stack: [],
    objects: buildObjectMap(buildGameObject({ id: 1 })),
    players: buildPlayers([0, 1]),
  });
}

describe("gameLoopController auto-pass authorization", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    dispatchAction.mockReset();
    dispatchResolveAll.mockReset();
    waitingForSubscriber = null;
    animationSpeedMultiplier = 1.0;
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("auto-passes when the local player controls another player's turn", async () => {
    const waitingFor = priority(1);
    storeState = {
      waitingFor,
      gameState: stateFor(waitingFor, 0),
      autoPassRecommended: true,
    };

    const controller = createGameLoopController({ mode: "local" });
    controller.start();

    await vi.advanceTimersByTimeAsync(200);

    expect(dispatchAction).toHaveBeenCalledWith({ type: "PassPriority" });
    controller.dispose();
  });

  it("does not auto-pass when another player controls the local player's turn", async () => {
    const waitingFor = priority(0);
    storeState = {
      waitingFor,
      gameState: stateFor(waitingFor, 1),
      autoPassRecommended: true,
    };

    const controller = createGameLoopController({ mode: "local" });
    controller.start();

    await vi.advanceTimersByTimeAsync(200);

    expect(dispatchAction).not.toHaveBeenCalled();
    controller.dispose();
  });

  it("cancels a scheduled local auto-pass when priority moves away from the local player", async () => {
    const humanPriority = priority(0);
    storeState = {
      waitingFor: humanPriority,
      gameState: {
        ...stateFor(humanPriority, 0),
        phase: "PostCombatMain",
      },
      autoPassRecommended: true,
    };

    const controller = createGameLoopController({ mode: "local" });
    controller.start();

    const aiPriority = priority(1);
    storeState = {
      waitingFor: aiPriority,
      gameState: {
        ...stateFor(aiPriority, 1),
        phase: "End",
      },
      autoPassRecommended: true,
    };
    waitingForSubscriber?.();

    await vi.advanceTimersByTimeAsync(200);

    expect(dispatchAction).not.toHaveBeenCalled();
    controller.dispose();
  });

  it("re-checks the engine auto-pass recommendation before firing a delayed auto-pass", async () => {
    // Phase-stop gating now lives in the engine, surfaced via `autoPassRecommended`
    // (a phase stop on the new phase flips it to false). The controller must
    // re-read the latest recommendation at fire time and cancel the delayed
    // auto-pass when the engine no longer recommends it.
    const waitingFor = priority(0);
    storeState = {
      waitingFor,
      gameState: {
        ...stateFor(waitingFor, 0),
        phase: "Upkeep",
      },
      autoPassRecommended: true,
    };

    const controller = createGameLoopController({ mode: "local" });
    controller.start();

    storeState = {
      waitingFor,
      gameState: {
        ...stateFor(waitingFor, 0),
        phase: "PreCombatMain",
      },
      // Engine now recommends against auto-pass (phase stop on PreCombatMain).
      autoPassRecommended: false,
    };

    await vi.advanceTimersByTimeAsync(200);

    expect(dispatchAction).not.toHaveBeenCalled();
    controller.dispose();
  });

  it("scales the auto-pass beat by the animation-speed multiplier (2x doubles the wait)", async () => {
    animationSpeedMultiplier = 2.0;
    const waitingFor = priority(1);
    storeState = {
      waitingFor,
      gameState: stateFor(waitingFor, 0),
      autoPassRecommended: true,
    };

    const controller = createGameLoopController({ mode: "local" });
    controller.start();

    // At the un-scaled 200ms beat the pass must NOT have fired yet — proves the
    // multiplier actually stretched the beat (reach-guard against a vacuous 0-beat).
    await vi.advanceTimersByTimeAsync(200);
    expect(dispatchAction).not.toHaveBeenCalled();

    // The doubled 400ms beat elapses and the pass dispatches.
    await vi.advanceTimersByTimeAsync(200);
    expect(dispatchAction).toHaveBeenCalledWith({ type: "PassPriority" });
    controller.dispose();
  });

  it("passes immediately (0ms beat) when the animation-speed multiplier is 0 without skipping the dispatch", async () => {
    animationSpeedMultiplier = 0;
    const waitingFor = priority(1);
    storeState = {
      waitingFor,
      gameState: stateFor(waitingFor, 0),
      autoPassRecommended: true,
    };

    const controller = createGameLoopController({ mode: "local" });
    controller.start();

    // Zero multiplier collapses the beat to 0ms but must still dispatch the pass.
    await vi.advanceTimersByTimeAsync(0);
    expect(dispatchAction).toHaveBeenCalledWith({ type: "PassPriority" });
    controller.dispose();
  });
});
