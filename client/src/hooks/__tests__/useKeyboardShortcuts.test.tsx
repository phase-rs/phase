import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";

import { useKeyboardShortcuts } from "../useKeyboardShortcuts";
import { useGameStore } from "../../stores/gameStore";
import { useUiStore } from "../../stores/uiStore";
import {
  buildGameState,
  buildManaPaymentWaitingFor,
  buildTargetSelectionProgress,
  buildTargetSelectionSlot,
  buildTriggerTargetSelectionWaitingFor,
} from "../../test/factories/gameStateFactory";

function KeyboardHarness() {
  useKeyboardShortcuts();
  return null;
}

describe("useKeyboardShortcuts", () => {
  beforeEach(() => {
    act(() => {
      useUiStore.setState({ selectedCardIds: [10, 20] });
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("escape skips an optional trigger target through the engine action", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = buildGameState({
      waiting_for: buildTriggerTargetSelectionWaitingFor({
        data: {
          player: 0,
          target_slots: [buildTargetSelectionSlot({ optional: true })],
          target_constraints: [],
          selection: buildTargetSelectionProgress(),
        },
      }),
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        undo: vi.fn(),
        stateHistory: [],
      });
    });

    render(<KeyboardHarness />);

    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });

    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: null },
    });
  });

  it("escape clears card-selection state when no engine targeting step is active", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = buildGameState();

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        undo: vi.fn(),
        stateHistory: [],
      });
    });

    render(<KeyboardHarness />);

    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });

    expect(useUiStore.getState().selectedCardIds).toEqual([]);
  });

  it("escape cancels mana payment through the engine action", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = buildGameState({
      waiting_for: buildManaPaymentWaitingFor(),
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        undo: vi.fn(),
        stateHistory: [],
      });
    });

    render(<KeyboardHarness />);

    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });

    expect(dispatch).toHaveBeenCalledWith({ type: "CancelCast" });
  });
});
