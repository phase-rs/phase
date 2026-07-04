import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ModalChoice, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameState, buildPendingCast } from "../../../test/factories/gameStateFactory.ts";
import { ModeChoiceModal } from "../ModeChoiceModal.tsx";

const dispatchMock = vi.fn();

function singleChoiceModal(): ModalChoice {
  return {
    min_choices: 1,
    max_choices: 1,
    mode_count: 2,
    mode_descriptions: ["You gain 2 life.", "You lose 2 life."],
    allow_repeat_modes: false,
  };
}

function setWaitingFor(waitingFor: WaitingFor) {
  const gameState = buildGameState({
    objects: {},
    priority_player: 0,
    waiting_for: waitingFor,
  });

  useGameStore.setState({
    gameState,
    waitingFor,
    dispatch: dispatchMock,
  });
}

describe("ModeChoiceModal", () => {
  beforeEach(() => {
    dispatchMock.mockReset();
    dispatchMock.mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
  });

  it("shows a Cancel affordance for an activated modal ability (CR 602.2b) and dispatches CancelCast", () => {
    setWaitingFor({
      type: "AbilityModeChoice",
      data: {
        player: 0,
        modal: singleChoiceModal(),
        source_id: 90,
        mode_abilities: [],
        is_activated: true,
      },
    });

    render(<ModeChoiceModal />);

    // Both mode rows render; single-choice modes auto-dispatch on click.
    expect(screen.getByText("You gain 2 life.")).toBeInTheDocument();
    const cancel = screen.getByRole("button", { name: "Cancel" });
    fireEvent.click(cancel);
    expect(dispatchMock).toHaveBeenCalledWith({ type: "CancelCast" });
  });

  it("hides the Cancel affordance for a triggered modal ability (CR 603.3c)", () => {
    setWaitingFor({
      type: "AbilityModeChoice",
      data: {
        player: 0,
        modal: singleChoiceModal(),
        source_id: 90,
        mode_abilities: [],
        is_activated: false,
      },
    });

    render(<ModeChoiceModal />);

    expect(screen.getByText("You gain 2 life.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
  });

  it("keeps the Cancel affordance for a modal spell (regression guard)", () => {
    setWaitingFor({
      type: "ModeChoice",
      data: {
        player: 0,
        modal: singleChoiceModal(),
        pending_cast: buildPendingCast({ object_id: 50 }),
      },
    });

    render(<ModeChoiceModal />);

    const cancel = screen.getByRole("button", { name: "Cancel" });
    fireEvent.click(cancel);
    expect(dispatchMock).toHaveBeenCalledWith({ type: "CancelCast" });
  });
});
