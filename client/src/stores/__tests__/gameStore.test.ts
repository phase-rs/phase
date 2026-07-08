import { act } from "react";
import { beforeEach, describe, expect, it } from "vitest";

import type { GameEvent } from "../../adapter/types";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory";
import {
  buildGameState,
  buildStackEntry,
} from "../../test/factories/gameStateFactory";
import { useGameStore } from "../gameStore";

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
    const state = buildGameState();
    const adapter = buildEngineAdapterMock(state);

    await act(() => useGameStore.getState().initGame("test-id", adapter));

    const store = useGameStore.getState();
    expect(store.adapter).toBe(adapter);
    expect(store.gameState).toEqual(state);
    expect(store.waitingFor).toEqual(state.waiting_for);
    expect(adapter.initialize).toHaveBeenCalled();
  });

  it("dispatch calls adapter.submitAction and updates state", async () => {
    const state1 = buildGameState({ turn_number: 1 });
    const state2 = buildGameState({ turn_number: 2 });
    const events: GameEvent[] = [{ type: "PriorityPassed", data: { player_id: 0 } }];

    const adapter = buildEngineAdapterMock(state1);
    await act(() => useGameStore.getState().initGame("test-id", adapter));

    // Update mock for next calls
    adapter.submitAction.mockResolvedValue({ events });
    adapter.getState.mockResolvedValue(state2);

    await act(() => useGameStore.getState().dispatch({ type: "PassPriority" }));

    const store = useGameStore.getState();
    expect(store.gameState).toEqual(state2);
    expect(store.events).toEqual(events);
    expect(adapter.submitAction).toHaveBeenCalledWith({ type: "PassPriority" }, 0);
  });

  it("dispatch pushes to stateHistory for undoable actions", async () => {
    const state1 = buildGameState({ turn_number: 1 });
    const state2 = buildGameState({ turn_number: 2 });
    const adapter = buildEngineAdapterMock(state1);

    await act(() => useGameStore.getState().initGame("test-id", adapter));
    adapter.getState.mockResolvedValue(state2);

    await act(() => useGameStore.getState().dispatch({ type: "PassPriority" }));

    expect(useGameStore.getState().stateHistory).toHaveLength(1);
    expect(useGameStore.getState().stateHistory[0]).toEqual(state1);
  });

  it("dispatch does not push to stateHistory when the stack is non-empty", async () => {
    // Even an undoable action like PassPriority must skip the checkpoint
    // while something is mid-resolution. Otherwise undoing later would
    // land the player back on a stack-with-stuff state instead of a clean
    // pre-trigger boundary.
    const triggerOnStack = buildStackEntry({
      id: 100,
      kind: {
        type: "TriggeredAbility",
        data: {
          source_id: 1,
          ability: { targets: [] },
        },
      },
    });
    const state1 = buildGameState({ turn_number: 1, stack: [triggerOnStack] });
    const state2 = buildGameState({ turn_number: 2 });
    const adapter = buildEngineAdapterMock(state1);

    await act(() => useGameStore.getState().initGame("test-id", adapter));
    adapter.getState.mockResolvedValue(state2);

    await act(() => useGameStore.getState().dispatch({ type: "PassPriority" }));

    expect(useGameStore.getState().stateHistory).toHaveLength(0);
  });

  it("dispatch does not push to stateHistory for revealed-info actions", async () => {
    const state1 = buildGameState();
    const state2 = buildGameState({ turn_number: 2 });
    const adapter = buildEngineAdapterMock(state1);

    await act(() => useGameStore.getState().initGame("test-id", adapter));
    adapter.getState.mockResolvedValue(state2);

    // PlayLand is NOT in UNDOABLE_ACTIONS
    await act(() =>
      useGameStore.getState().dispatch({ type: "PlayLand", data: { object_id: 10, card_id: 1 } }),
    );

    expect(useGameStore.getState().stateHistory).toHaveLength(0);
  });

  it("undo restores previous state from stateHistory", async () => {
    const state1 = buildGameState({ turn_number: 1 });
    const state2 = buildGameState({ turn_number: 2 });
    const adapter = buildEngineAdapterMock(state1);

    await act(() => useGameStore.getState().initGame("test-id", adapter));
    adapter.getState.mockResolvedValue(state2);

    await act(() => useGameStore.getState().dispatch({ type: "PassPriority" }));
    expect(useGameStore.getState().gameState?.turn_number).toBe(2);

    await act(() => useGameStore.getState().undo());

    const store = useGameStore.getState();
    expect(store.gameState?.turn_number).toBe(1);
    expect(store.stateHistory).toHaveLength(0);
    expect(store.events).toEqual([]);
    expect(adapter.restoreState).toHaveBeenCalledWith(state1);
  });

  it("undo calls adapter.restoreState with previous state", async () => {
    const state1 = buildGameState({ turn_number: 1 });
    const state2 = buildGameState({ turn_number: 2 });
    const adapter = buildEngineAdapterMock(state1);

    await act(() => useGameStore.getState().initGame("test-id", adapter));
    adapter.getState.mockResolvedValue(state2);

    await act(() => useGameStore.getState().dispatch({ type: "PassPriority" }));

    act(() => useGameStore.getState().undo());

    expect(adapter.restoreState).toHaveBeenCalledOnce();
    expect(adapter.restoreState).toHaveBeenCalledWith(state1);
  });

  it("undo with no adapter does nothing", () => {
    // Set stateHistory but no adapter
    act(() => {
      useGameStore.setState({
        stateHistory: [buildGameState()],
        adapter: null,
      });
    });
    act(() => useGameStore.getState().undo());
    // Should not crash; stateHistory unchanged
    expect(useGameStore.getState().stateHistory).toHaveLength(1);
  });

  it("undo is unavailable when stateHistory is empty", async () => {
    const state = buildGameState();
    const adapter = buildEngineAdapterMock(state);
    await act(() => useGameStore.getState().initGame("test-id", adapter));

    act(() => useGameStore.getState().undo());
    expect(adapter.restoreState).not.toHaveBeenCalled();
  });

  it("limits stateHistory to MAX_UNDO_HISTORY entries", async () => {
    const states = Array.from({ length: 7 }, (_, i) =>
      buildGameState({ turn_number: i }),
    );
    const adapter = buildEngineAdapterMock(states[0]);

    await act(() => useGameStore.getState().initGame("test-id", adapter));

    for (let i = 1; i < states.length; i++) {
      adapter.getState.mockResolvedValue(states[i]);
      await act(() =>
        useGameStore.getState().dispatch({ type: "PassPriority" }),
      );
    }

    // Should be capped at 5
    expect(useGameStore.getState().stateHistory).toHaveLength(5);
  });

  it("dispatch does not push to stateHistory in multiplayer", async () => {
    // Authoritative state lives on the wire in multiplayer, so undo is
    // suppressed — rewinding a single client's view would desync.
    const state1 = buildGameState({ turn_number: 1 });
    const state2 = buildGameState({ turn_number: 2 });
    const adapter = buildEngineAdapterMock(state1);

    await act(() => useGameStore.getState().initGame("test-id", adapter));
    act(() => useGameStore.getState().setGameMode("online"));
    adapter.getState.mockResolvedValue(state2);

    await act(() => useGameStore.getState().dispatch({ type: "PassPriority" }));

    expect(useGameStore.getState().stateHistory).toHaveLength(0);
  });

  it("undo is a no-op in multiplayer even if stateHistory is non-empty", async () => {
    // Defense-in-depth: setGameMode after history was populated would be
    // unusual, but the guard must still hold.
    const state1 = buildGameState({ turn_number: 1 });
    const adapter = buildEngineAdapterMock(state1);

    await act(() => useGameStore.getState().initGame("test-id", adapter));
    act(() => {
      useGameStore.setState({ stateHistory: [state1], gameMode: "p2p-host" });
    });

    await act(() => useGameStore.getState().undo());

    // History untouched; restoreState never invoked.
    expect(useGameStore.getState().stateHistory).toHaveLength(1);
    expect(adapter.restoreState).not.toHaveBeenCalled();
  });

  it("reset clears all state", async () => {
    const state = buildGameState();
    const adapter = buildEngineAdapterMock(state);

    await act(() => useGameStore.getState().initGame("test-id", adapter));
    act(() => useGameStore.getState().reset());

    const store = useGameStore.getState();
    expect(store.gameState).toBeNull();
    expect(store.adapter).toBeNull();
    expect(store.stateHistory).toEqual([]);
    expect(adapter.dispose).toHaveBeenCalled();
  });
});
