import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction, WaitingFor } from "../../../adapter/types.ts";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { isClickThroughWaitingFor } from "../DialogHost.tsx";
import { AnnouncingOpponentModalContent } from "../AnnouncingOpponentModal.tsx";
import { announcerElectionOf } from "../announcerElection.ts";

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

function renderModal(waitingFor: WaitingFor) {
  const dispatch = vi.fn<(action: GameAction) => void>();
  const election = announcerElectionOf(waitingFor);
  if (!election) throw new Error("expected an announcer election");
  render(
    <AnnouncingOpponentModalContent
      election={election}
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

  // CR 601.2c + CR 115.1 (CR 707.12): a copy announcement carries the same
  // election on its walk; the modal renders it and dispatches the same action,
  // and the board overlay stands aside.
  it("renders a copy announcement's election", () => {
    useMultiplayerStore.setState({
      playerNames: new Map([
        [1, "Alice"],
        [2, "Bob"],
      ]),
    });
    const copyElection: WaitingFor = {
      type: "CopyRetarget",
      data: {
        player: 0,
        copy_id: 40,
        target_slots: [],
        current_slot: 0,
        mode: "Announce",
        picks: [],
        can_keep_rest: false,
        announcer_election: {
          candidates: [2, 1],
          choice_index: 2,
          choice_count: 2,
          target_type: "Creature",
        },
      },
    };
    expect(isClickThroughWaitingFor(copyElection)).toBe(false);
    const dispatch = renderModal(copyElection);
    expect(
      screen.getByRole("heading", { name: "Choose Announcing Opponent (2 of 2)" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Alice" }));
    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseAnnouncingOpponent",
      data: { opponent: 1 },
    });
  });

  it("is no election for a copy walk answering a target slot", () => {
    const slotPrompt: WaitingFor = {
      type: "CopyRetarget",
      data: {
        player: 0,
        copy_id: 40,
        target_slots: [{ legal_alternatives: [{ Object: 61 }] }],
        current_slot: 0,
      },
    };
    expect(announcerElectionOf(slotPrompt)).toBeNull();
    expect(isClickThroughWaitingFor(slotPrompt)).toBe(true);
  });
});
