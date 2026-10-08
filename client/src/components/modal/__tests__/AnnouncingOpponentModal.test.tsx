import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction, WaitingFor } from "../../../adapter/types.ts";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { AnnouncingOpponentModalContent } from "../AnnouncingOpponentModal.tsx";

type AnnouncingOpponentWaitingFor = Extract<
  WaitingFor,
  { type: "ChooseAnnouncingOpponent" }
>;

function announcingOpponentWaitingFor(): AnnouncingOpponentWaitingFor {
  return {
    type: "ChooseAnnouncingOpponent",
    data: {
      player: 0,
      candidates: [2, 1],
      choice_index: 1,
      choice_count: 2,
      target_type: "Land",
      pending_cast: {},
    },
  };
}

function renderModal(waitingFor: AnnouncingOpponentWaitingFor) {
  const dispatch = vi.fn<(action: GameAction) => void>();
  render(
    <AnnouncingOpponentModalContent
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

describe("AnnouncingOpponentModalContent", () => {
  it("registers the waiting state as handled", () => {
    expect(isWaitingForHandled(announcingOpponentWaitingFor())).toBe(true);
  });

  it("dispatches the selected announcing opponent", () => {
    useMultiplayerStore.setState({
      playerNames: new Map([
        [1, "Alice"],
        [2, "Bob"],
      ]),
    });
    const dispatch = renderModal(announcingOpponentWaitingFor());

    expect(
      screen.getByRole("heading", { name: "Choose Announcing Opponent (1 of 2)" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Choose which opponent announces the land target (1 of 2)."),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Bob" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseAnnouncingOpponent",
      data: { opponent: 2 },
    });
  });
});
