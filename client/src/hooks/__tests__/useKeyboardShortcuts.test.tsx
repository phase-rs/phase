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
import type { GameAction, GameEvent } from "../../adapter/types";

function KeyboardHarness() {
  useKeyboardShortcuts();
  return null;
}

function ClickModeHarness({ sandboxToolsAvailable }: { sandboxToolsAvailable?: boolean }) {
  useKeyboardShortcuts({ sandboxToolsAvailable });
  return null;
}

function seedClickModeCase() {
  const gameState = buildGameState();
  act(() => {
    useUiStore.setState({
      debugInteractionMode: false,
      debugContextMenu: null,
      helpSheetOpen: false,
    });
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      dispatch: vi.fn().mockResolvedValue([]),
      undo: vi.fn(),
      stateHistory: [],
    });
  });
}

function pressKey(init: KeyboardEventInit) {
  act(() => {
    window.dispatchEvent(new KeyboardEvent("keydown", init));
  });
}

describe("useKeyboardShortcuts", () => {
  beforeEach(() => {
    act(() => {
      useUiStore.setState({ selectedCardIds: [10, 20] });
    });
  });

  afterEach(() => {
    cleanup();
    useUiStore.setState({ helpSheetOpen: false });
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

  it("t dispatches the engine-authored mana-payment shortcut actions serially", async () => {
    let resolveFirst!: (events: GameEvent[]) => void;
    const firstDispatch = new Promise<GameEvent[]>(resolve => {
      resolveFirst = resolve;
    });
    const dispatch = vi
      .fn()
      .mockImplementationOnce(() => firstDispatch)
      .mockResolvedValueOnce([]);
    const gameState = buildGameState({
      waiting_for: buildManaPaymentWaitingFor(),
    });
    const shortcutActions: GameAction[] = [17, 23].map((objectId) => ({
      type: "TapLandForMana",
      data: {
        selection: {
          source: { object_id: objectId, incarnation: 1 },
          ability_index: null,
          mana_type: "Green",
          output: { type: "Concrete", data: "Green" },
          atomic_combination: null,
          restrictions: [],
          penalty: "None",
          taps_for_mana: [],
        },
      },
    }));

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        legalActions: [{ type: "PassPriority" }],
        manaPaymentShortcutActions: shortcutActions,
        dispatch,
        undo: vi.fn(),
        stateHistory: [],
      });
    });

    render(<KeyboardHarness />);

    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "T" }));
    });

    expect(dispatch).toHaveBeenCalledTimes(1);
    expect(dispatch).toHaveBeenNthCalledWith(1, shortcutActions[0]);

    await act(async () => {
      resolveFirst([]);
      await firstDispatch;
    });

    expect(dispatch).toHaveBeenCalledTimes(2);
    expect(dispatch).toHaveBeenNthCalledWith(2, shortcutActions[1]);
  });

  it("k toggles click mode when sandbox tools are available", () => {
    seedClickModeCase();
    render(<ClickModeHarness sandboxToolsAvailable />);

    pressKey({ key: "k" });
    expect(useUiStore.getState().debugInteractionMode).toBe(true);

    pressKey({ key: "K" });
    expect(useUiStore.getState().debugInteractionMode).toBe(false);

    act(() => {
      useUiStore.setState({
        debugInteractionMode: false,
        debugContextMenu: { objectId: 1, x: 0, y: 0, surface: "game" },
      });
    });
    pressKey({ key: "k" });
    expect(useUiStore.getState().debugInteractionMode).toBe(true);
    expect(useUiStore.getState().debugContextMenu).toBeNull();
  });

  it("k is inert when sandbox tools are unavailable", () => {
    seedClickModeCase();
    render(<KeyboardHarness />);

    pressKey({ key: "k" });
    expect(useUiStore.getState().debugInteractionMode).toBe(false);

    // Reach guard: the same listener handles "?" — dispatched last so the open
    // help sheet cannot short-circuit the "k" above.
    pressKey({ key: "?" });
    expect(useUiStore.getState().helpSheetOpen).toBe(true);
  });

  it("k ignores Ctrl, Meta and Alt", () => {
    seedClickModeCase();
    render(<ClickModeHarness sandboxToolsAvailable />);

    pressKey({ key: "k", ctrlKey: true });
    expect(useUiStore.getState().debugInteractionMode).toBe(false);
    pressKey({ key: "k", metaKey: true });
    expect(useUiStore.getState().debugInteractionMode).toBe(false);
    pressKey({ key: "k", altKey: true });
    expect(useUiStore.getState().debugInteractionMode).toBe(false);

    // Reach guard: plain K in the same harness toggles the mode.
    pressKey({ key: "k" });
    expect(useUiStore.getState().debugInteractionMode).toBe(true);
  });
});
