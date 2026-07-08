import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { PriorityYieldList } from "../PriorityYieldList.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";

const { dispatchActionMock } = vi.hoisted(() => ({ dispatchActionMock: vi.fn() }));
vi.mock("../../../game/dispatch.ts", () => ({ dispatchAction: dispatchActionMock }));

function seed(yieldCount: number) {
  const gameState = buildGameState({
    objects: buildObjectMap(
      buildGameObject({ id: 50, card_id: 9, name: "Bloodghast", zone: "Battlefield" }),
    ),
    priority_yields: Array.from({ length: yieldCount }, (_, i) => ({
      player: 0,
      target: { AllCopies: { card_id: 100 + i } },
    })),
  });
  act(() => {
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
  });
}

describe("PriorityYieldList", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    dispatchActionMock.mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("renders nothing when the viewer holds no yields", () => {
    seed(0);
    const { container } = render(<PriorityYieldList />);
    expect(container).toBeEmptyDOMElement();
  });

  it("collapses standing yields into a single fixed-footprint chip that shows the count", () => {
    seed(3);
    render(<PriorityYieldList />);

    // Fixed footprint: one chip regardless of yield count. The list rows and the
    // clear-all live behind it, not inline, so the action rail height is constant.
    expect(screen.getByText("3")).toBeInTheDocument();
    expect(screen.queryByText("Clear all")).not.toBeInTheDocument();
    expect(screen.queryByText("Revoke")).not.toBeInTheDocument();
  });

  it("reveals the revocable list only after the chip is opened", () => {
    seed(2);
    render(<PriorityYieldList />);

    fireEvent.click(screen.getByRole("button", { name: /auto-passing/i }));

    expect(screen.getByText("Clear all")).toBeInTheDocument();
    expect(screen.getAllByText("Revoke")).toHaveLength(2);
  });

  it("dispatches a scoped Remove when a row's Revoke is chosen", () => {
    seed(1);
    render(<PriorityYieldList />);

    fireEvent.click(screen.getByRole("button", { name: /auto-passing/i }));
    fireEvent.click(screen.getByText("Revoke"));

    expect(dispatchActionMock).toHaveBeenCalledWith({
      type: "SetPriorityYield",
      data: { op: { type: "Remove", data: { target: { AllCopies: { card_id: 100 } } } } },
    });
  });

  it("dispatches ClearAll and closes the popover when Clear all is chosen", () => {
    seed(2);
    render(<PriorityYieldList />);

    fireEvent.click(screen.getByRole("button", { name: /auto-passing/i }));
    fireEvent.click(screen.getByText("Clear all"));

    expect(dispatchActionMock).toHaveBeenCalledWith({
      type: "SetPriorityYield",
      data: { op: { type: "ClearAll" } },
    });
    expect(screen.queryByText("Clear all")).not.toBeInTheDocument();
  });
});
