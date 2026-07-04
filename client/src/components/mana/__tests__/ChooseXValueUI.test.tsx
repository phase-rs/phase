import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { ChooseXValueUI } from "../ChooseXValueUI";
import { DialogHost } from "../../modal/DialogHost.tsx";
import { useGameStore } from "../../../stores/gameStore";
import type { GameState, PendingCast, WaitingFor } from "../../../adapter/types";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildChooseXValueWaitingFor,
  buildGameState,
  buildManaPaymentWaitingFor,
  buildPendingCast,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return buildGameState({
    objects: buildObjectMap(
      buildGameObjectWithCoreTypes(["Instant"], {
        id: 42,
        card_id: 1,
        name: "Nature's Rhythm",
        zone: "Stack",
      }),
    ),
    next_object_id: 100,
    waiting_for: buildManaPaymentWaitingFor(),
    has_pending_cast: true,
    ...overrides,
  });
}

function createPendingCast(): PendingCast {
  return buildPendingCast({
    object_id: 42,
    card_id: 1,
    cost: { type: "Cost", shards: ["X", "G", "G"], generic: 0 },
  });
}

function chooseXWaitingFor(max: number, min?: number): WaitingFor {
  return buildChooseXValueWaitingFor({
    data: {
      player: 0,
      max,
      ...(min === undefined ? {} : { min }),
      pending_cast: createPendingCast(),
    },
  });
}

describe("ChooseXValueUI", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  afterEach(() => {
    cleanup();
  });

  it("renders nothing when not in ChooseXValue state", () => {
    setGameStoreForTest({
      gameState: createGameState(),
      waitingFor: buildPriorityWaitingFor(),
    });

    const { container } = render(<ChooseXValueUI />);
    expect(container).toBeEmptyDOMElement();
  });

  it("shows card name and dispatches ChooseX with selected value on confirm", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const waitingFor = chooseXWaitingFor(5);

    setGameStoreForTest({
      gameState: createGameState({ waiting_for: waitingFor }),
      waitingFor,
      dispatch,
    });

    render(<ChooseXValueUI />);

    expect(screen.getByText(/Choose a value for X/)).toBeInTheDocument();
    expect(screen.getByText(/Nature's Rhythm/)).toBeInTheDocument();

    const slider = screen.getByLabelText("Choose X value") as HTMLInputElement;
    expect(slider.min).toBe("0");
    expect(slider.max).toBe("5");

    fireEvent.change(slider, { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: "Confirm X = 3" }));

    expect(dispatch).toHaveBeenCalledWith({ type: "ChooseX", data: { value: 3 } });
  });

  it("dispatches CancelCast when cancel is clicked", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const waitingFor = chooseXWaitingFor(3);

    setGameStoreForTest({
      gameState: createGameState({ waiting_for: waitingFor }),
      waitingFor,
      dispatch,
    });

    render(<ChooseXValueUI />);

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(dispatch).toHaveBeenCalledWith({ type: "CancelCast" });
  });

  it("honors min value and resets to a valid value when ChooseXValue state re-enters", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const waitingFor = chooseXWaitingFor(10, 1);

    setGameStoreForTest({
      gameState: createGameState({ waiting_for: waitingFor }),
      waitingFor,
      dispatch,
    });

    const { rerender } = render(<ChooseXValueUI />);

    const slider = screen.getByLabelText("Choose X value") as HTMLInputElement;
    expect(slider.min).toBe("1");
    expect(screen.getByRole("button", { name: "Confirm X = 1" })).toBeInTheDocument();
    fireEvent.change(slider, { target: { value: "7" } });
    expect(screen.getByRole("button", { name: "Confirm X = 7" })).toBeInTheDocument();

    // Simulate re-entering ChooseXValue (e.g., after cost reduction changes max)
    const nextWaitingFor = chooseXWaitingFor(4, 2);
    setGameStoreForTest({
      gameState: createGameState({ waiting_for: nextWaitingFor }),
      waitingFor: nextWaitingFor,
      dispatch,
    });

    rerender(<ChooseXValueUI />);

    expect(screen.getByRole("button", { name: "Confirm X = 2" })).toBeInTheDocument();
  });

  it("renders nothing for impossible min greater than max bounds", () => {
    const waitingFor = chooseXWaitingFor(0, 1);

    setGameStoreForTest({
      gameState: createGameState({ waiting_for: waitingFor }),
      waitingFor,
    });

    const { container } = render(<ChooseXValueUI />);
    expect(container).toBeEmptyDOMElement();
  });

  it("range slider accepts input when mounted inside DialogHost (#2427)", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const waitingFor = chooseXWaitingFor(5);

    setGameStoreForTest({
      gameState: createGameState({
        turn_decision_controller: 0,
        active_player: 0,
        waiting_for: waitingFor,
      }),
      waitingFor,
      dispatch,
    });

    render(
      <DialogHost>
        <ChooseXValueUI />
      </DialogHost>,
    );

    const slider = screen.getByLabelText("Choose X value") as HTMLInputElement;
    fireEvent.change(slider, { target: { value: "4" } });
    expect(screen.getByRole("button", { name: "Confirm X = 4" })).toBeInTheDocument();
  });
});
