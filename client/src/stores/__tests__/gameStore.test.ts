import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { EngineAdapter, GameEvent, GameState } from "../../adapter/types";
import { useGameStore } from "../gameStore";

function createMockState(overrides: Partial<GameState> = {}): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [],
    priority_player: 0,
    objects: {},
    next_object_id: 1,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 42,
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
    ...overrides,
  };
}

function createMockAdapter(state: GameState): EngineAdapter {
  return {
    initialize: vi.fn().mockResolvedValue(undefined),
    initializeGame: vi.fn().mockResolvedValue([]),
    submitAction: vi.fn().mockResolvedValue([]),
    getState: vi.fn().mockResolvedValue(state),
    restoreState: vi.fn(),
    dispose: vi.fn(),
  };
}

describe("gameStore", () => {
  beforeEach(() => {
    act(() => {
      useGameStore.setState({
        gameState: null,
        events: [],
        adapter: null,
        waitingFor: null,
        stateHistory: [],
      });
    });
  });

  it("initializes with null gameState", () => {
    const { gameState, adapter, waitingFor, stateHistory } =
      useGameStore.getState();
    expect(gameState).toBeNull();
    expect(adapter).toBeNull();
    expect(waitingFor).toBeNull();
    expect(stateHistory).toEqual([]);
  });

  it("initGame sets adapter and creates initial game state", async () => {
    const state = createMockState();
    const adapter = createMockAdapter(state);

    await act(() => useGameStore.getState().initGame(adapter));

    const store = useGameStore.getState();
    expect(store.adapter).toBe(adapter);
    expect(store.gameState).toEqual(state);
    expect(store.waitingFor).toEqual(state.waiting_for);
    expect(adapter.initialize).toHaveBeenCalled();
  });

  it("dispatch calls adapter.submitAction and updates state", async () => {
    const state1 = createMockState({ turn_number: 1 });
    const state2 = createMockState({ turn_number: 2 });
    const events: GameEvent[] = [{ type: "PriorityPassed", data: { player_id: 0 } }];

    const adapter = createMockAdapter(state1);
    await act(() => useGameStore.getState().initGame(adapter));

    // Update mock for next calls
    (adapter.submitAction as ReturnType<typeof vi.fn>).mockResolvedValue(events);
    (adapter.getState as ReturnType<typeof vi.fn>).mockResolvedValue(state2);

    await act(() => useGameStore.getState().dispatch({ type: "PassPriority" }));

    const store = useGameStore.getState();
    expect(store.gameState).toEqual(state2);
    expect(store.events).toEqual(events);
    expect(adapter.submitAction).toHaveBeenCalledWith({ type: "PassPriority" });
  });

  it("dispatch pushes to stateHistory for undoable actions", async () => {
    const state1 = createMockState({ turn_number: 1 });
    const state2 = createMockState({ turn_number: 2 });
    const adapter = createMockAdapter(state1);

    await act(() => useGameStore.getState().initGame(adapter));
    (adapter.getState as ReturnType<typeof vi.fn>).mockResolvedValue(state2);

    await act(() => useGameStore.getState().dispatch({ type: "PassPriority" }));

    expect(useGameStore.getState().stateHistory).toHaveLength(1);
    expect(useGameStore.getState().stateHistory[0]).toEqual(state1);
  });

  it("dispatch does not push to stateHistory for revealed-info actions", async () => {
    const state1 = createMockState();
    const state2 = createMockState({ turn_number: 2 });
    const adapter = createMockAdapter(state1);

    await act(() => useGameStore.getState().initGame(adapter));
    (adapter.getState as ReturnType<typeof vi.fn>).mockResolvedValue(state2);

    // PlayLand is NOT in UNDOABLE_ACTIONS
    await act(() =>
      useGameStore.getState().dispatch({ type: "PlayLand", data: { card_id: 1 } }),
    );

    expect(useGameStore.getState().stateHistory).toHaveLength(0);
  });

  it("undo restores previous state from stateHistory", async () => {
    const state1 = createMockState({ turn_number: 1 });
    const state2 = createMockState({ turn_number: 2 });
    const adapter = createMockAdapter(state1);

    await act(() => useGameStore.getState().initGame(adapter));
    (adapter.getState as ReturnType<typeof vi.fn>).mockResolvedValue(state2);

    await act(() => useGameStore.getState().dispatch({ type: "PassPriority" }));
    expect(useGameStore.getState().gameState?.turn_number).toBe(2);

    act(() => useGameStore.getState().undo());

    const store = useGameStore.getState();
    expect(store.gameState?.turn_number).toBe(1);
    expect(store.stateHistory).toHaveLength(0);
    expect(store.events).toEqual([]);
    expect(adapter.restoreState).toHaveBeenCalledWith(state1);
  });

  it("undo calls adapter.restoreState with previous state", async () => {
    const state1 = createMockState({ turn_number: 1 });
    const state2 = createMockState({ turn_number: 2 });
    const adapter = createMockAdapter(state1);

    await act(() => useGameStore.getState().initGame(adapter));
    (adapter.getState as ReturnType<typeof vi.fn>).mockResolvedValue(state2);

    await act(() => useGameStore.getState().dispatch({ type: "PassPriority" }));

    act(() => useGameStore.getState().undo());

    expect(adapter.restoreState).toHaveBeenCalledOnce();
    expect(adapter.restoreState).toHaveBeenCalledWith(state1);
  });

  it("undo with no adapter does nothing", () => {
    // Set stateHistory but no adapter
    act(() => {
      useGameStore.setState({
        stateHistory: [createMockState()],
        adapter: null,
      });
    });
    act(() => useGameStore.getState().undo());
    // Should not crash; stateHistory unchanged
    expect(useGameStore.getState().stateHistory).toHaveLength(1);
  });

  it("undo is unavailable when stateHistory is empty", async () => {
    const state = createMockState();
    const adapter = createMockAdapter(state);
    await act(() => useGameStore.getState().initGame(adapter));

    act(() => useGameStore.getState().undo());
    expect(adapter.restoreState).not.toHaveBeenCalled();
  });

  it("limits stateHistory to MAX_UNDO_HISTORY entries", async () => {
    const states = Array.from({ length: 7 }, (_, i) =>
      createMockState({ turn_number: i }),
    );
    const adapter = createMockAdapter(states[0]);

    await act(() => useGameStore.getState().initGame(adapter));

    for (let i = 1; i < states.length; i++) {
      (adapter.getState as ReturnType<typeof vi.fn>).mockResolvedValue(states[i]);
      await act(() =>
        useGameStore.getState().dispatch({ type: "PassPriority" }),
      );
    }

    // Should be capped at 5
    expect(useGameStore.getState().stateHistory).toHaveLength(5);
  });

  it("reset clears all state", async () => {
    const state = createMockState();
    const adapter = createMockAdapter(state);

    await act(() => useGameStore.getState().initGame(adapter));
    act(() => useGameStore.getState().reset());

    const store = useGameStore.getState();
    expect(store.gameState).toBeNull();
    expect(store.adapter).toBeNull();
    expect(store.stateHistory).toEqual([]);
    expect(adapter.dispose).toHaveBeenCalled();
  });
});
