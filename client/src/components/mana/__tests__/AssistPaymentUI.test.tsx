import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type { GameState, WaitingFor } from "../../../adapter/types";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { useGameStore } from "../../../stores/gameStore";
import {
  buildAssistPaymentWaitingFor,
  buildGameState,
  buildManaPaymentWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { AssistPaymentUI } from "../AssistPaymentUI.tsx";

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return buildGameState({
    next_object_id: 100,
    waiting_for: buildManaPaymentWaitingFor(),
    has_pending_cast: true,
    ...overrides,
  });
}

// caster = 0, chosen = 0 so the local PLAYER_ID (0) is the acting helper and
// `useCanActForWaitingState` returns true (CR 702.132a routes to `chosen`).
function assistPaymentWaitingFor(max: number): WaitingFor {
  return buildAssistPaymentWaitingFor({
    data: { caster: 1, chosen: 0, max_generic: max },
  });
}

describe("AssistPaymentUI", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  afterEach(() => {
    cleanup();
  });

  it("registers the waiting state as handled", () => {
    expect(isWaitingForHandled(assistPaymentWaitingFor(3))).toBe(true);
  });

  it("renders nothing when not in AssistPayment state", () => {
    setGameStoreForTest({
      gameState: createGameState(),
    });

    const { container } = render(<AssistPaymentUI />);
    expect(container).toBeEmptyDOMElement();
  });

  it("clamps the slider to [0, max] and defaults to 0", () => {
    const waitingFor = assistPaymentWaitingFor(4);
    setGameStoreForTest({
      gameState: createGameState({ waiting_for: waitingFor }),
      waitingFor,
    });

    render(<AssistPaymentUI />);

    const slider = screen.getByRole("slider") as HTMLInputElement;
    expect(slider.min).toBe("0");
    expect(slider.max).toBe("4");
    expect(slider.value).toBe("0");
  });

  it("dispatches CommitAssistPayment with the selected value", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const waitingFor = assistPaymentWaitingFor(4);
    setGameStoreForTest({
      gameState: createGameState({ waiting_for: waitingFor }),
      waitingFor,
      dispatch,
    });

    render(<AssistPaymentUI />);

    fireEvent.change(screen.getByRole("slider"), { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button"));

    expect(dispatch).toHaveBeenCalledWith({
      type: "CommitAssistPayment",
      data: { generic: 3 },
    });
  });

  it("commits generic:0 when paying nothing", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const waitingFor = assistPaymentWaitingFor(4);
    setGameStoreForTest({
      gameState: createGameState({ waiting_for: waitingFor }),
      waitingFor,
      dispatch,
    });

    render(<AssistPaymentUI />);

    fireEvent.click(screen.getByRole("button"));

    expect(dispatch).toHaveBeenCalledWith({
      type: "CommitAssistPayment",
      data: { generic: 0 },
    });
  });
});
