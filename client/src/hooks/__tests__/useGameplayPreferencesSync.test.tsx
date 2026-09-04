import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { EngineAdapter, GameState } from "../../adapter/types";
import { dispatchActionForGameSession } from "../../game/dispatch";
import { useGameplayPreferencesSync } from "../useGameplayPreferencesSync";
import {
  nextGameSessionGeneration,
  useGameStore,
} from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useUiStore } from "../../stores/uiStore";

vi.mock("../../game/dispatch", () => ({ dispatchActionForGameSession: vi.fn() }));

const adapter = {} as EngineAdapter;
const gameState = {} as GameState;

describe("useGameplayPreferencesSync", () => {
  beforeEach(() => {
    vi.mocked(dispatchActionForGameSession).mockClear();
    usePreferencesStore.setState({
      phaseStops: [],
      priorityPassingMode: "Standard",
    });
    useGameStore.setState({
      adapter: null,
      gameState: null,
      engineCommitEpoch: 0,
      gameSessionGeneration: nextGameSessionGeneration(),
    });
    useUiStore.setState({ fullControl: false });
  });

  afterEach(() => cleanup());

  it("waits for an installed snapshot, dedupes, and resends for a fresh lifecycle", async () => {
    renderHook(() => useGameplayPreferencesSync());

    act(() => {
      useGameStore.setState({ adapter });
    });
    expect(dispatchActionForGameSession).not.toHaveBeenCalled();

    act(() => {
      useGameStore.setState({ gameState, engineCommitEpoch: 1 });
    });
    await waitFor(() => expect(dispatchActionForGameSession).toHaveBeenCalledTimes(2));
    expect(dispatchActionForGameSession).toHaveBeenNthCalledWith(
      1,
      { type: "SetPhaseStops", data: { stops: [] } },
      adapter,
      expect.any(Number),
    );
    expect(dispatchActionForGameSession).toHaveBeenNthCalledWith(
      2,
      { type: "SetPriorityPassingMode", data: { mode: "Standard" } },
      adapter,
      expect.any(Number),
    );

    act(() => {
      useGameStore.setState({ engineCommitEpoch: 2 });
    });
    expect(dispatchActionForGameSession).toHaveBeenCalledTimes(2);

    act(() => {
      usePreferencesStore.getState().setPhaseStops([]);
      usePreferencesStore.getState().setPriorityPassingMode("Standard");
    });
    expect(dispatchActionForGameSession).toHaveBeenCalledTimes(2);

    act(() => usePreferencesStore.getState().setPriorityPassingMode("SkipLowUseWindows"));
    await waitFor(() => {
      expect(dispatchActionForGameSession).toHaveBeenLastCalledWith(
        { type: "SetPriorityPassingMode", data: { mode: "SkipLowUseWindows" } },
        adapter,
        expect.any(Number),
      );
      expect(dispatchActionForGameSession).toHaveBeenCalledTimes(3);
    });

    const firstGeneration = useGameStore.getState().gameSessionGeneration;
    act(() => {
      useGameStore.setState({ gameSessionGeneration: nextGameSessionGeneration() });
    });
    expect(useGameStore.getState().gameSessionGeneration).toBeGreaterThan(firstGeneration);
    await waitFor(() => expect(dispatchActionForGameSession).toHaveBeenCalledTimes(5));

    const secondGeneration = useGameStore.getState().gameSessionGeneration;
    act(() => {
      useGameStore.setState({ gameSessionGeneration: nextGameSessionGeneration() });
    });
    expect(useGameStore.getState().gameSessionGeneration).toBeGreaterThan(secondGeneration);
    await waitFor(() => expect(dispatchActionForGameSession).toHaveBeenCalledTimes(7));
  });

  it("retries rejected sends without duplicating successful preferences", async () => {
    vi.mocked(dispatchActionForGameSession)
      .mockRejectedValueOnce(new Error("transient"))
      .mockResolvedValue(undefined);
    useGameStore.setState({ adapter, gameState, engineCommitEpoch: 1 });
    renderHook(() => useGameplayPreferencesSync());

    await waitFor(() => expect(dispatchActionForGameSession).toHaveBeenCalledTimes(1));

    act(() => usePreferencesStore.getState().setPhaseStops([]));
    await waitFor(() => expect(dispatchActionForGameSession).toHaveBeenCalledTimes(3));
    expect(dispatchActionForGameSession).toHaveBeenNthCalledWith(
      2,
      { type: "SetPhaseStops", data: { stops: [] } },
      adapter,
      expect.any(Number),
    );
    expect(dispatchActionForGameSession).toHaveBeenNthCalledWith(
      3,
      { type: "SetPriorityPassingMode", data: { mode: "Standard" } },
      adapter,
      expect.any(Number),
    );

    act(() => usePreferencesStore.getState().setPhaseStops([]));
    await waitFor(() => expect(dispatchActionForGameSession).toHaveBeenCalledTimes(3));
  });

  it("overrides the synced mode while Full Control is on, and restores it when off", async () => {
    useGameStore.setState({ adapter, gameState, engineCommitEpoch: 1 });
    renderHook(() => useGameplayPreferencesSync());
    await waitFor(() => expect(dispatchActionForGameSession).toHaveBeenCalledTimes(2));
    expect(dispatchActionForGameSession).toHaveBeenLastCalledWith(
      { type: "SetPriorityPassingMode", data: { mode: "Standard" } },
      adapter,
      expect.any(Number),
    );

    // Full Control lives in `uiStore`, not `preferencesStore`, so it reaches the
    // engine only through its own subscription.
    act(() => useUiStore.getState().toggleFullControl());
    await waitFor(() => {
      expect(dispatchActionForGameSession).toHaveBeenLastCalledWith(
        { type: "SetPriorityPassingMode", data: { mode: "FullControl" } },
        adapter,
        expect.any(Number),
      );
    });

    // An unrelated `uiStore` write must not re-dispatch: the subscription is a
    // plain (non-selector) one guarded on the previous `fullControl` value.
    const callsAfterOverride = vi.mocked(dispatchActionForGameSession).mock.calls.length;
    act(() => useUiStore.setState({ selectedCardIds: [] }));
    expect(dispatchActionForGameSession).toHaveBeenCalledTimes(callsAfterOverride);

    act(() => useUiStore.getState().toggleFullControl());
    await waitFor(() => {
      expect(dispatchActionForGameSession).toHaveBeenLastCalledWith(
        { type: "SetPriorityPassingMode", data: { mode: "Standard" } },
        adapter,
        expect.any(Number),
      );
    });
  });
});
