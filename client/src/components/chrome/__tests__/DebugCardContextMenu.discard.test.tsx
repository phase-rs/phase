import { act, cleanup, render, screen, within, fireEvent } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useUiStore } from "../../../stores/uiStore";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory";
import { gameStateFactory } from "../../../test/factories/gameStateFactory";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers";
import { DebugCardContextMenu } from "../DebugCardContextMenu";

const dispatchDebug = vi.fn();

vi.mock("../../../hooks/useGameDispatch", () => ({
  useGameDispatch: () => dispatchDebug,
}));

vi.mock("../../../hooks/useIsMobile", () => ({
  useIsMobile: () => false,
}));

function openMenuFor(objectId: number) {
  act(() => {
    useUiStore.getState().openDebugContextMenu({
      objectId,
      x: 40,
      y: 40,
      surface: "game",
    });
  });
}

describe("DebugCardContextMenu discard item", () => {
  beforeEach(() => {
    const inHand = gameObjectFactory
      .creature(2, 2)
      .inHand()
      .withId(11)
      .named("Grizzly Bears")
      .build();
    const onBattlefield = gameObjectFactory
      .creature(2, 2)
      .onBattlefield()
      .withId(12)
      .named("Runeclaw Bear")
      .build();
    setGameStoreForTest({
      gameState: gameStateFactory
        .withPlayers(0, 1)
        .withObjects(inHand, onBattlefield)
        // Settled-Priority projection: the engine lists the hand card as discardable.
        .build({ derived: { debug_discardable_hand_cards: [11] } }),
    });
    act(() => {
      useUiStore.setState({ debugContextMenu: null, selectedObjectId: null });
    });
    dispatchDebug.mockClear();
  });

  afterEach(() => {
    cleanup();
    act(() => {
      useUiStore.setState({ debugContextMenu: null, selectedObjectId: null });
    });
  });

  it("dispatches a Discard debug action for a hand card", () => {
    render(<DebugCardContextMenu surface="game" />);
    openMenuFor(11);

    const menu = screen.getByRole("menu", { name: "Grizzly Bears" });
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Discard" }));

    expect(dispatchDebug).toHaveBeenCalledWith({
      type: "Debug",
      data: { type: "Discard", data: { object_id: 11 } },
    });
  });

  it("does not offer Discard for a battlefield card", () => {
    render(<DebugCardContextMenu surface="game" />);
    openMenuFor(12);

    const menu = screen.getByRole("menu", { name: "Runeclaw Bear" });
    // Reach guard: the battlefield-only Sacrifice item is present.
    expect(
      within(menu).getByRole("menuitem", { name: "Sacrifice" }),
    ).toBeInTheDocument();
    expect(
      within(menu).queryByRole("menuitem", { name: "Discard" }),
    ).not.toBeInTheDocument();
  });

  it("does not offer Discard for a hand card the engine does not project as discardable", () => {
    // Replacement-choice projection: the same hand card, but the engine's list is empty.
    setGameStoreForTest({
      gameState: gameStateFactory
        .withPlayers(0, 1)
        .withObjects(
          gameObjectFactory
            .creature(2, 2)
            .inHand()
            .withId(11)
            .named("Grizzly Bears")
            .build(),
        )
        .build({ derived: {} }),
    });
    render(<DebugCardContextMenu surface="game" />);
    openMenuFor(11);

    const menu = screen.getByRole("menu", { name: "Grizzly Bears" });
    // Reach guard: the menu rendered and another item is still present.
    expect(
      within(menu).getByRole("menuitem", { name: "Remove" }),
    ).toBeInTheDocument();
    expect(
      within(menu).queryByRole("menuitem", { name: "Discard" }),
    ).not.toBeInTheDocument();
  });
});
