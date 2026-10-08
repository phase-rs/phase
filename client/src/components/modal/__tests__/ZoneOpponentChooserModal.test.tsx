import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import i18n from "i18next";
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

  it("asks for the next player, labelling the chooser as You, when ordering per-player choices", () => {
    useMultiplayerStore.setState({
      playerNames: new Map([[1, "Alice"]]),
    });
    const dispatch = renderModal({
      type: "ChooseFromZoneOpponentChooser",
      data: {
        player: 0,
        candidates: [0, 1],
        ability: {},
        purpose: "PerPlayerChoiceOrder",
      },
    });

    expect(
      screen.getByRole("heading", { name: "Choose Next Player" }),
    ).toBeInTheDocument();
    expect(
      screen.getAllByRole("button").map((button) => button.textContent),
    ).toEqual(["You", "Alice"]);
    fireEvent.click(screen.getByRole("button", { name: "Alice" }));
    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseZoneOpponentChooser",
      data: { opponent: 1 },
    });
  });

  it("asks who makes the choice when electing a substitute", () => {
    useMultiplayerStore.setState({
      playerNames: new Map([[2, "Bob"]]),
    });
    const dispatch = renderModal({
      type: "ChooseFromZoneOpponentChooser",
      data: {
        player: 1,
        candidates: [1, 2],
        ability: {},
        purpose: "SubstituteChooser",
      },
    });

    expect(
      screen.getByRole("heading", { name: "Choose Player" }),
    ).toBeInTheDocument();
    expect(
      screen.getAllByRole("button").map((button) => button.textContent),
    ).toEqual(["You", "Bob"]);
    fireEvent.click(screen.getByRole("button", { name: "Bob" }));
    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseZoneOpponentChooser",
      data: { opponent: 2 },
    });
  });

  it("labels the chooser with the translated self label", async () => {
    i18n.addResourceBundle("de", "game", { player: { you: "Du" } }, true, true);
    await i18n.changeLanguage("de");
    try {
      useMultiplayerStore.setState({ playerNames: new Map([[1, "Alice"]]) });
      renderModal({
        type: "ChooseFromZoneOpponentChooser",
        data: {
          player: 0,
          candidates: [0, 1],
          ability: {},
          purpose: "PerPlayerChoiceOrder",
        },
      });
      expect(
        screen.getAllByRole("button").map((button) => button.textContent),
      ).toEqual(["Du", "Alice"]);
    } finally {
      await i18n.changeLanguage("en");
    }
  });
});
