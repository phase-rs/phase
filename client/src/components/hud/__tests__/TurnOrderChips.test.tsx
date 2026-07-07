import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { TurnOrderChips } from "../TurnOrderChips.tsx";

describe("TurnOrderChips", () => {
  beforeEach(() => {
    useGameStore.setState({ gameState: buildGameState() });
  });

  afterEach(() => {
    cleanup();
  });

  it("hides without derived, empty, or matching turn-order rows", () => {
    const { rerender } = render(<TurnOrderChips playerId={0} />);

    expect(screen.queryByTestId("turn-order-chips-0")).not.toBeInTheDocument();

    useGameStore.setState({
      gameState: buildGameState({ derived: { turn_order: [] } }),
    });
    rerender(<TurnOrderChips playerId={0} />);

    expect(screen.queryByTestId("turn-order-chips-0")).not.toBeInTheDocument();

    useGameStore.setState({
      gameState: buildGameState({
        derived: { turn_order: [{ player: 1, slot_index: 0, turns_from_now: 0 }] },
      }),
    });
    rerender(<TurnOrderChips playerId={0} />);

    expect(screen.queryByTestId("turn-order-chips-0")).not.toBeInTheDocument();
  });

  it("renders all matching rows in slot order", () => {
    useGameStore.setState({
      gameState: buildGameState({
        derived: {
          turn_order: [
            { player: 0, slot_index: 1, turns_from_now: 1 },
            { player: 0, slot_index: 0, turns_from_now: 0 },
            { player: 0, slot_index: 2, turns_from_now: 2 },
          ],
        },
      }),
    });

    render(<TurnOrderChips playerId={0} />);

    expect(screen.getByTitle("Current turn")).toHaveTextContent("NOW");
    expect(screen.getByTitle("Next turn")).toHaveTextContent("NEXT");
    expect(screen.getByTitle("2 turns from now")).toHaveTextContent("+2");
    expect(screen.getByTestId("turn-order-chips-0")).toHaveTextContent("NOWNEXT+2");
  });
});
