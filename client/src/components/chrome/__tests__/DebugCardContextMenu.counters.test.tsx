import { act } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types";
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

function openMenuFor(card: GameObject) {
  setGameStoreForTest({
    gameState: gameStateFactory.withPlayers(0, 1).withObjects(card).build(),
  });
  render(<DebugCardContextMenu surface="game" />);
  act(() => {
    useUiStore.getState().openDebugContextMenu({
      objectId: card.id,
      x: 80,
      y: 120,
      surface: "game",
    });
  });
}

function modifyCounters(objectId: number, counterType: string, delta: number) {
  return {
    type: "Debug",
    data: {
      type: "ModifyCounters",
      data: { object_id: objectId, counter_type: counterType, delta },
    },
  };
}

describe("DebugCardContextMenu counter rows", () => {
  beforeEach(() => {
    act(() => {
      useUiStore.setState({ debugContextMenu: null });
    });
    dispatchDebug.mockClear();
  });

  afterEach(() => {
    cleanup();
    act(() => {
      useUiStore.setState({ debugContextMenu: null });
    });
  });

  it("offers +1/+1 and -1/-1 on a creature with no counters", () => {
    openMenuFor(
      gameObjectFactory
        .creature(2, 2)
        .onBattlefield()
        .named("Grizzly Bears")
        .withId(7)
        .build(),
    );

    const removePlus = screen.getByRole("menuitem", { name: "+1/+1: −1" });
    const removeMinus = screen.getByRole("menuitem", { name: "-1/-1: −1" });
    expect(removePlus).toBeDisabled();
    expect(removeMinus).toBeDisabled();
    fireEvent.click(removePlus);
    fireEvent.click(removeMinus);
    expect(dispatchDebug).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("menuitem", { name: "+1/+1: +1" }));
    expect(dispatchDebug).toHaveBeenCalledWith(modifyCounters(7, "P1P1", 1));
    fireEvent.click(screen.getByRole("menuitem", { name: "-1/-1: +1" }));
    expect(dispatchDebug).toHaveBeenCalledWith(modifyCounters(7, "M1M1", 1));
  });

  it("does not offer P/T counters on a noncreature without them", () => {
    openMenuFor(
      gameObjectFactory
        .artifact()
        .onBattlefield()
        .named("Sol Ring")
        .withId(8)
        .build(),
    );

    expect(screen.getByRole("menuitem", { name: "Tap" })).toBeInTheDocument();
    expect(
      screen.queryByRole("menuitem", { name: "+1/+1: +1" }),
    ).not.toBeInTheDocument();
  });

  it("keeps a present +1/+1 row on a noncreature", () => {
    openMenuFor(
      gameObjectFactory
        .artifact()
        .onBattlefield()
        .named("Sol Ring")
        .withId(8)
        .params({ counters: { P1P1: 1 } })
        .build(),
    );

    const removePlus = screen.getByRole("menuitem", { name: "+1/+1: −1" });
    expect(removePlus).toBeEnabled();
    expect(removePlus.parentElement).toHaveTextContent("1");
    expect(
      screen.getAllByRole("menuitem", { name: "+1/+1: +1" }),
    ).toHaveLength(1);
  });

  it("lists fixed P/T rows once plus other present counters", () => {
    openMenuFor(
      gameObjectFactory
        .creature(2, 2)
        .onBattlefield()
        .named("Grizzly Bears")
        .withId(7)
        .params({ counters: { P1P1: 3, stun: 2, lore: 0 } })
        .build(),
    );

    expect(
      screen.getAllByRole("menuitem", { name: "+1/+1: +1" }),
    ).toHaveLength(1);
    expect(
      screen.getByRole("menuitem", { name: "+1/+1: −1" }).parentElement,
    ).toHaveTextContent("3");
    expect(
      screen.getByRole("menuitem", { name: "-1/-1: −1" }).parentElement,
    ).toHaveTextContent("0");
    expect(screen.getByRole("menuitem", { name: "-1/-1: +1" })).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "stun: +1" })).toBeInTheDocument();
    expect(
      screen.queryByRole("menuitem", { name: "lore: +1" }),
    ).not.toBeInTheDocument();
  });
});
