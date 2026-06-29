import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, WaitingFor } from "../../../adapter/types";

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

vi.mock("../../../stores/preferencesStore", () => ({
  usePreferencesStore: {
    getState: () => ({ aiSeats: [] }),
  },
}));

vi.mock("../../../stores/uiStore", () => ({
  useUiStore: {
    getState: () => ({ fullControl: false }),
  },
}));

import { createGameLoopController } from "../gameLoopController";

function priority(player: number): WaitingFor {
  return { type: "Priority", data: { player } } as WaitingFor;
}

function stateFor(waitingFor: WaitingFor, priorityPlayer: number): GameState {
  return {
    waiting_for: waitingFor,
    priority_player: priorityPlayer,
    phase: "PreCombatMain",
    stack: [],
    objects: { 1: { id: 1 } },
    players: [{ id: 0 }, { id: 1 }],
  } as unknown as GameState;
}

describe("gameLoopController auto-pass authorization", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    dispatchAction.mockReset();
    dispatchResolveAll.mockReset();
    waitingForSubscriber = null;
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

  it("re-checks phase stops before firing a delayed auto-pass", async () => {
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
        phase_stops: { 0: ["PreCombatMain"] },
      },
      autoPassRecommended: true,
    };

    await vi.advanceTimersByTimeAsync(200);

    expect(dispatchAction).not.toHaveBeenCalled();
    controller.dispose();
  });
});
