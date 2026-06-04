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

vi.mock("../../../stores/gameStore", () => ({
  useGameStore: {
    getState: () => storeState,
    subscribe: () => () => {},
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
});
