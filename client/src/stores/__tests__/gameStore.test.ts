import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ActionRejection, EngineAdapter, GameEvent, GameState } from "../../adapter/types";
import { actionRejectionError } from "../../adapter/types";
import { useAppNotificationStore } from "../../stores/appToastStore";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory";
import {
  buildGameState,
  buildStackEntry,
} from "../../test/factories/gameStateFactory";
import type { GameMode } from "../gameStore";
import {
  canExportAuthoritativeState,
  hasRemoteHumans,
  isAuthorityRemote,
  useGameStore,
} from "../gameStore";

describe("game mode classification", () => {
  // The two questions the old `isMultiplayerMode` answered with one bit:
  //   authority — does the authoritative engine state live off this client?
  //   company   — do humans on OTHER clients share this game?
  // `native-ai` (desktop solo vs the phase-server sidecar) is the mode where
  // they disagree, which is exactly what the merged predicate could not say.
  const EXPECTED: Record<GameMode, { authority: boolean; company: boolean }> = {
    "ai": { authority: false, company: false },
    "local": { authority: false, company: false },
    "native-ai": { authority: true, company: false },
    "online": { authority: true, company: true },
    "p2p-host": { authority: true, company: true },
    "p2p-join": { authority: true, company: true },
    "draft-match": { authority: true, company: true },
    "spectate": { authority: true, company: true },
  };

  it.each(Object.keys(EXPECTED) as GameMode[])(
    "classifies %s on both axes",
    (mode) => {
      expect(isAuthorityRemote(mode)).toBe(EXPECTED[mode].authority);
      expect(hasRemoteHumans(mode)).toBe(EXPECTED[mode].company);
    },
  );

  it("answers the two questions differently for native-ai", () => {
    // Non-vacuity: these two assertions are contradictory for ANY single
    // merged predicate. `isMultiplayerMode` — and equally the rejected
    // `isMultiplayerMode(mode) || mode === "native-ai"` shape — returns one
    // value for this input and therefore fails one of them whichever way it
    // answers. Only a genuine split can satisfy both.
    expect(isAuthorityRemote("native-ai")).toBe(true);
    expect(hasRemoteHumans("native-ai")).toBe(false);
  });

  it("treats hot-seat local as solo despite having two humans", () => {
    // The row a careless "solo means one human" reading gets wrong: two
    // humans share one client and one state, so there is no peer to desync.
    expect(hasRemoteHumans("local")).toBe(false);
    expect(isAuthorityRemote("local")).toBe(false);
  });

  it.each(["ai", "local", "native-ai", "p2p-host"] as GameMode[])(
    "allows authoritative bug reports when %s owns the state",
    (mode) => {
      expect(canExportAuthoritativeState(mode)).toBe(true);
    },
  );

  it.each(["online", "p2p-join", "draft-match", "spectate"] as GameMode[])(
    "blocks authoritative bug reports when %s only has a redacted view",
    (mode) => {
      expect(canExportAuthoritativeState(mode)).toBe(false);
    },
  );

  it("answers false to both for the pre-game null mode", () => {
    expect(isAuthorityRemote(null)).toBe(false);
    expect(hasRemoteHumans(null)).toBe(false);
  });
});

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
      useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
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

  it("binds the adapter before initializeGame can publish an initial remote snapshot", async () => {
    const state = buildGameState();
    let adapterDuringInitialization: EngineAdapter | null = null;
    const adapter = buildEngineAdapterMock(state, {
      initializeGame: vi.fn(async () => {
        adapterDuringInitialization = useGameStore.getState().adapter;
        return { events: [] };
      }),
    });

    await act(() => useGameStore.getState().initGame("test-id", adapter));

    expect(adapterDuringInitialization).toBe(adapter);
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

  it("surfaces a direct structured rejection at the first rendered related object", async () => {
    const state = buildGameState();
    const adapter = buildEngineAdapterMock(state);
    await act(() => useGameStore.getState().initGame("test-id", adapter));
    const first = document.createElement("div");
    first.dataset.objectId = "200";
    first.getBoundingClientRect = () => ({
      x: -20, y: 10, top: 10, right: 20, bottom: 50, left: -20, width: 40, height: 40,
      toJSON: () => ({}),
    });
    const second = document.createElement("div");
    second.dataset.objectId = "201";
    document.body.append(first, second);
    const rejection: ActionRejection = {
      code: "invalid_action",
      disposition: "invalid",
      message: "Engine error: ObjectId(200) must be blocked by 2 or more creatures",
      related_object_ids: [200, 201],
    };
    adapter.submitAction.mockRejectedValue(actionRejectionError(rejection));

    await expect(useGameStore.getState().dispatch({ type: "PassPriority" })).rejects.toMatchObject({
      rejection,
    });

    expect(useAppNotificationStore.getState().notification).toEqual({
      title: "Action failed",
      description: rejection.message,
      anchor: { x: 192, y: 62, placement: "below" },
    });
    first.remove();
    second.remove();
  });

  it("silently drops a stale structured rejection from the direct dispatcher", async () => {
    const state = buildGameState();
    const adapter = buildEngineAdapterMock(state);
    await act(() => useGameStore.getState().initGame("test-id", adapter));
    adapter.submitAction.mockRejectedValue(actionRejectionError({
      code: "stale_action",
      disposition: "stale",
      message: "This action is no longer current",
      related_object_ids: [200],
    }));

    await expect(useGameStore.getState().dispatch({ type: "PassPriority" })).resolves.toEqual([]);
    expect(useAppNotificationStore.getState().notification).toBeNull();
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
    // Model a real engine: `restoreState` actually rewinds it, so the read that
    // follows returns the restored state. `undo` commits the snapshot's own
    // post-restore state (post-restore, the engine is the source of truth and
    // both halves of the pair must come from it), so a mock whose reads ignored
    // `restoreState` would be lying about the engine.
    adapter.restoreState.mockImplementation((restored: GameState) => {
      adapter.getState.mockResolvedValue(restored);
    });

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

  it("undo is a no-op for native-ai, whose authority is the sidecar", async () => {
    // Regression guard for the predicate split: `native-ai` is solo but
    // wire-authoritative, so the rename must NOT have leaked client-side
    // rewind into it. `WebSocketAdapter.restoreState` throws on this
    // transport, so a leak here would surface as a thrown adapter call.
    const state1 = buildGameState({ turn_number: 1 });
    const adapter = buildEngineAdapterMock(state1);

    await act(() => useGameStore.getState().initGame("test-id", adapter));
    act(() => {
      useGameStore.setState({ stateHistory: [state1], gameMode: "native-ai" });
    });

    await act(() => useGameStore.getState().undo());

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
