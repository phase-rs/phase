import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameEvent, GameState } from "../../adapter/types";
import { useAnimationStore } from "../../stores/animationStore";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useGameDispatch } from "../useGameDispatch";

// Mock the normalizer
vi.mock("../../animation/eventNormalizer", () => ({
  normalizeEvents: vi.fn((events: GameEvent[]) =>
    events.length > 0
      ? [{ effects: [{ event: { type: "DamageDealt", data: { source_id: 1, target: { Object: 2 }, amount: 3 } }, duration: 100 }], duration: 100 }]
      : [],
  ),
}));

const mockEvents: GameEvent[] = [
  { type: "DamageDealt", data: { amount: 3, source_id: 1, target: { Object: 2 } } } as unknown as GameEvent,
];

const mockState = {
  waiting_for: null,
  turn: { active_player: 0 },
} as unknown as GameState;

const mockAdapter = {
  initialize: vi.fn(),
  initializeGame: vi.fn(),
  submitAction: vi.fn().mockResolvedValue({ events: mockEvents }),
  getState: vi.fn().mockResolvedValue(mockState),
  getLegalActions: vi.fn().mockResolvedValue([]),
  restoreState: vi.fn(),
  getAiAction: vi.fn().mockReturnValue(null),
  dispose: vi.fn(),
};

describe("useGameDispatch", () => {
  beforeEach(() => {
    vi.useFakeTimers();

    // Set up gameStore with a mock adapter and initial state
    useGameStore.setState({
      adapter: mockAdapter,
      gameState: { waiting_for: null } as unknown as GameState,
      events: [],
      eventHistory: [],
      stateHistory: [],
      waitingFor: null,
    });

    useAnimationStore.getState().clearQueue();
    usePreferencesStore.setState({ animationSpeed: "normal" });

    vi.clearAllMocks();
    mockAdapter.submitAction.mockResolvedValue({ events: mockEvents });
    mockAdapter.getState.mockResolvedValue(mockState);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("calls adapter.submitAction with the action", async () => {
    const { result } = renderHook(() => useGameDispatch());
    const action = { type: "PassPriority" as const };

    await act(async () => {
      const promise = result.current(action);
      await vi.runAllTimersAsync();
      await promise;
    });

    expect(mockAdapter.submitAction).toHaveBeenCalledWith(action);
  });

  it("defers state update until after animation duration", async () => {
    const { result } = renderHook(() => useGameDispatch());
    const action = { type: "PassPriority" as const };

    let resolved = false;
    await act(async () => {
      const promise = result.current(action).then(() => {
        resolved = true;
      });

      // submitAction resolves immediately, but state update waits for animation
      await vi.advanceTimersByTimeAsync(50);
      // Animation is 100ms at normal speed, so not resolved yet at 50ms
      expect(resolved).toBe(false);

      await vi.advanceTimersByTimeAsync(60);
      await promise;
      expect(resolved).toBe(true);
    });

    // State should be updated now
    expect(useGameStore.getState().gameState).toBe(mockState);
  });

  it("skips animation wait when speed is instant", async () => {
    usePreferencesStore.setState({ animationSpeed: "instant" });

    const { result } = renderHook(() => useGameDispatch());
    const action = { type: "PassPriority" as const };

    await act(async () => {
      await result.current(action);
    });

    // State should be updated immediately (no timer needed)
    expect(useGameStore.getState().gameState).toBe(mockState);
    expect(useAnimationStore.getState().queue).toHaveLength(0);
  });

  it("serializes rapid dispatches", async () => {
    const { result } = renderHook(() => useGameDispatch());
    const action1 = { type: "PassPriority" as const };
    const action2 = { type: "PassPriority" as const };

    const callOrder: number[] = [];
    mockAdapter.submitAction
      .mockImplementationOnce(async () => {
        callOrder.push(1);
        return { events: mockEvents };
      })
      .mockImplementationOnce(async () => {
        callOrder.push(2);
        return { events: mockEvents };
      });

    await act(async () => {
      const p1 = result.current(action1);
      const p2 = result.current(action2);

      // First dispatch animates
      await vi.advanceTimersByTimeAsync(110);
      await p1;

      // Second dispatch should now be processing
      await vi.advanceTimersByTimeAsync(110);
      await p2;
    });

    // Both should have been called, in order
    expect(callOrder).toEqual([1, 2]);
    expect(mockAdapter.submitAction).toHaveBeenCalledTimes(2);
  });

  it("updates state with events in eventHistory", async () => {
    const { result } = renderHook(() => useGameDispatch());
    const action = { type: "PassPriority" as const };

    await act(async () => {
      const promise = result.current(action);
      await vi.runAllTimersAsync();
      await promise;
    });

    expect(useGameStore.getState().eventHistory).toEqual(mockEvents);
    expect(useGameStore.getState().events).toEqual(mockEvents);
  });
});
