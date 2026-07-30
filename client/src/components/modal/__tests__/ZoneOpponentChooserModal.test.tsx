import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction, WaitingFor } from "../../../adapter/types.ts";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { ZoneOpponentChooserModalContent } from "../ZoneOpponentChooserModal.tsx";

type ZoneOpponentChooserWaitingFor = Extract<
  WaitingFor,
  { type: "ChooseFromZoneOpponentChooser" }
>;

function zoneOpponentChooserWaitingFor(): ZoneOpponentChooserWaitingFor {
  return {
    type: "ChooseFromZoneOpponentChooser",
    data: {
      player: 0,
      candidates: [2, 1],
      ability: {},
    },
  };
}

function renderModal(waitingFor: ZoneOpponentChooserWaitingFor) {
  const dispatch = vi.fn<(action: GameAction) => void>();
  render(
    <ZoneOpponentChooserModalContent waitingFor={waitingFor} dispatch={dispatch} />,
  );
  return dispatch;
}

afterEach(() => {
  cleanup();
  useMultiplayerStore.setState({ playerNames: new Map() });
});

describe("ZoneOpponentChooserModalContent", () => {
  it("registers the waiting state as handled", () => {
    expect(isWaitingForHandled(zoneOpponentChooserWaitingFor())).toBe(true);
  });

  it("dispatches the selected choosing opponent", () => {
    useMultiplayerStore.setState({
      playerNames: new Map([
        [1, "Alice"],
        [2, "Bob"],
      ]),
    });
    const dispatch = renderModal(zoneOpponentChooserWaitingFor());

    expect(screen.getByRole("heading", { name: "Choose Opponent" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Bob" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseZoneOpponentChooser",
      data: { opponent: 2 },
    });
  });

  it("renders candidates in the engine-supplied order", () => {
    // Candidate ordering is game ordering and belongs to the engine — the
    // client must not re-sort it. The fixture lists [2, 1]; the buttons must
    // appear in exactly that order.
    useMultiplayerStore.setState({
      playerNames: new Map([
        [1, "Alice"],
        [2, "Bob"],
      ]),
    });
    renderModal(zoneOpponentChooserWaitingFor());

    const labels = screen
      .getAllByRole("button")
      .map((button) => button.textContent);
    expect(labels).toEqual(["Bob", "Alice"]);
  });
});
