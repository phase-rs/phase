import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { EngineAdapter, GameEvent, GameState } from "../../adapter/types";
import { GAME_CHECKPOINTS_PREFIX, GAME_KEY_PREFIX } from "../../constants/storage";
import { useGameStore } from "../../stores/gameStore";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory";
import { buildGameState, buildPriorityWaitingFor } from "../../test/factories/gameStateFactory";
import { restoreGameState } from "../dispatch";

vi.mock("idb-keyval", () => ({
  createStore: vi.fn(() => ({})),
  del: vi.fn().mockResolvedValue(undefined),
  get: vi.fn().mockResolvedValue(undefined),
  set: vi.fn().mockResolvedValue(undefined),
}));

import { set as idbSet } from "idb-keyval";

function createMockState(overrides: Partial<GameState> = {}): GameState {
  return buildGameState({
    players: [],
    rng_seed: 42,
    waiting_for: buildPriorityWaitingFor(),
    ...overrides,
  });
}

function createMockAdapter(state: GameState): EngineAdapter {
  let currentState = state;
  return buildEngineAdapterMock(state, {
    getState: vi.fn().mockImplementation(() => Promise.resolve(currentState)),
    restoreState: vi.fn().mockImplementation(async (nextState: GameState) => {
      currentState = nextState;
    }),
  });
}

describe("restoreGameState", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useGameStore.setState({
      gameId: null,
      gameMode: null,
      gameState: null,
      events: [],
      eventHistory: [],
      logHistory: [],
      nextLogSeq: 0,
      adapter: null,
      waitingFor: null,
      legalActions: [],
      autoPassRecommended: false,
      spellCosts: {},
      legalActionsByObject: {},
      stateHistory: [],
      turnCheckpoints: [],
      lobbyProgress: null,
    });
  });

  it("replaces active game state, clears stale histories, and persists the restored state", async () => {
    const oldState = createMockState({ turn_number: 3 });
    const importedState = createMockState({ turn_number: 9 });
    const adapter = createMockAdapter(oldState);
    const event: GameEvent = { type: "PriorityPassed", data: { player_id: 0 } };

    useGameStore.setState({
      gameId: "debug-import",
      adapter,
      gameState: oldState,
      waitingFor: oldState.waiting_for,
      events: [event],
      eventHistory: [event],
      logHistory: [{
        seq: 0,
        turn: 3,
        phase: "PreCombatMain",
        category: "Game",
        segments: [{ type: "Text", value: "old log" }],
      }],
      nextLogSeq: 1,
      stateHistory: [oldState],
      turnCheckpoints: [oldState],
    });

    let err: string | null = "not run";
    await act(async () => {
      err = await restoreGameState(importedState);
    });

    const store = useGameStore.getState();
    expect(err).toBeNull();
    expect(adapter.restoreState).toHaveBeenCalledWith(importedState);
    expect(store.gameState).toEqual(importedState);
    expect(store.waitingFor).toEqual(importedState.waiting_for);
    expect(store.events).toEqual([]);
    expect(store.eventHistory).toEqual([]);
    expect(store.logHistory).toEqual([]);
    expect(store.nextLogSeq).toBe(0);
    expect(store.stateHistory).toEqual([]);
    expect(store.turnCheckpoints).toEqual([]);
    expect(idbSet).toHaveBeenCalledWith(
      GAME_KEY_PREFIX + "debug-import",
      importedState,
      expect.anything(),
    );
    expect(idbSet).toHaveBeenCalledWith(
      GAME_CHECKPOINTS_PREFIX + "debug-import",
      [],
      expect.anything(),
    );
  });

  it("preserves turn checkpoints when requested", async () => {
    const oldState = createMockState({ turn_number: 3 });
    const checkpoint = createMockState({ turn_number: 2 });
    const restoredState = createMockState({ turn_number: 1 });
    const adapter = createMockAdapter(oldState);

    useGameStore.setState({
      gameId: "debug-checkpoint",
      adapter,
      gameState: oldState,
      waitingFor: oldState.waiting_for,
      turnCheckpoints: [checkpoint],
    });

    let err: string | null = "not run";
    await act(async () => {
      err = await restoreGameState(restoredState, { preserveCheckpoints: true });
    });

    const store = useGameStore.getState();
    expect(err).toBeNull();
    expect(store.turnCheckpoints).toEqual([checkpoint]);
    expect(idbSet).toHaveBeenCalledWith(
      GAME_CHECKPOINTS_PREFIX + "debug-checkpoint",
      [checkpoint],
      expect.anything(),
    );
  });
});
