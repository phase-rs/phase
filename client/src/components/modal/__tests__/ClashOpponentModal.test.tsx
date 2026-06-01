import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction, WaitingFor } from "../../../adapter/types.ts";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { ClashOpponentModalContent } from "../ClashOpponentModal.tsx";

type ClashOpponentWaitingFor = Extract<WaitingFor, { type: "ClashChooseOpponent" }>;

function clashOpponentWaitingFor(): ClashOpponentWaitingFor {
  return {
    type: "ClashChooseOpponent",
    data: {
      player: 0,
      candidates: [2, 1],
      ability: {},
    },
  };
}

function renderModal(waitingFor: ClashOpponentWaitingFor) {
  const dispatch = vi.fn<(action: GameAction) => void>();
  render(
    <ClashOpponentModalContent
      waitingFor={waitingFor}
      seatOrder={[0, 1, 2]}
      dispatch={dispatch}
    />,
  );
  return dispatch;
}

afterEach(() => {
  cleanup();
  useMultiplayerStore.setState({ playerNames: new Map() });
});

describe("ClashOpponentModalContent", () => {
  it("registers the waiting state as handled", () => {
    expect(isWaitingForHandled(clashOpponentWaitingFor())).toBe(true);
  });

  it("dispatches the selected clash opponent", () => {
    useMultiplayerStore.setState({
      playerNames: new Map([
        [1, "Alice"],
        [2, "Bob"],
      ]),
    });
    const dispatch = renderModal(clashOpponentWaitingFor());

    expect(screen.getByRole("heading", { name: "Choose Clash Opponent" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Bob" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseClashOpponent",
      data: { opponent: 2 },
    });
  });
});
