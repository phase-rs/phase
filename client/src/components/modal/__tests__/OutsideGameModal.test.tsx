import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function outsideGameChoice(count: number): WaitingFor {
  return {
    type: "OutsideGameChoice",
    data: {
      player: 0,
      source_id: 1,
      choices: [
        {
          source: {
            type: "Sideboard",
            data: { sideboard_index: 0, card: { name: "Pyroclasm" } },
          },
          count,
          name: "Pyroclasm",
        },
      ],
      count: 1,
      reveal: true,
      up_to: false,
      destination: "Hand",
    },
  };
}

// CR 406.3: Face-up exile candidate, addressed by `ObjectId`. Used to verify
// the modal renders exile rows distinguishably and dispatches the
// `OutsideGameSelection::FaceUpExile` wire shape.
function outsideGameChoiceWithExile(): WaitingFor {
  return {
    type: "OutsideGameChoice",
    data: {
      player: 0,
      source_id: 1,
      choices: [
        {
          source: {
            type: "Sideboard",
            data: { sideboard_index: 0, card: { name: "Pyroclasm" } },
          },
          count: 1,
          name: "Pyroclasm",
        },
        {
          source: { type: "FaceUpExile", data: { object_id: 42 } },
          count: 1,
          name: "Pithing Needle",
        },
      ],
      count: 1,
      reveal: true,
      up_to: false,
      destination: "Hand",
    },
  };
}

function setWaitingFor(waitingFor: WaitingFor) {
  useGameStore.setState({
    gameMode: "online",
    gameState: buildGameState({ waiting_for: waitingFor, next_object_id: 100 }),
    waitingFor,
  });
}

describe("OutsideGameModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("clears selected copies synchronously when a new outside-game prompt replaces the old one", () => {
    setWaitingFor(outsideGameChoice(2));
    const { rerender } = render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: /Pyroclasm/ }));
    expect(screen.getByRole("button", { name: "Confirm" })).toBeEnabled();

    setWaitingFor(outsideGameChoice(1));
    rerender(<CardChoiceModal />);

    expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();
    expect(dispatchMock).not.toHaveBeenCalled();
  });

  it("renders a face-up exile candidate distinguishably and dispatches FaceUpExile on confirm", () => {
    // CR 406.3 + CR 400.11: Karn-class disjunction exposes both source pools
    // in a single choice list; selecting the exile candidate must produce an
    // `OutsideGameSelection::FaceUpExile` on the wire.
    setWaitingFor(outsideGameChoiceWithExile());
    render(<CardChoiceModal />);

    expect(screen.getByText("Pyroclasm")).toBeInTheDocument();
    expect(screen.getByText("Pithing Needle")).toBeInTheDocument();
    expect(screen.getByText("From exile")).toBeInTheDocument();
    expect(screen.getByText("From sideboard")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Pithing Needle/ }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledTimes(1);
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseOutsideGameCards",
      data: {
        selections: [{ type: "FaceUpExile", data: { object_id: 42 } }],
      },
    });
  });
});
