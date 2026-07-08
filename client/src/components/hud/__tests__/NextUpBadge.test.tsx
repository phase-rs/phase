import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { NextUpBadge } from "../NextUpBadge.tsx";

describe("NextUpBadge", () => {
  beforeEach(() => {
    useGameStore.setState({ gameState: buildGameState() });
  });

  afterEach(() => {
    cleanup();
  });

  it("hides unless the player owns the next actual turn slot", () => {
    const { rerender } = render(<NextUpBadge playerId={0} />);

    expect(screen.queryByText("Next Up")).not.toBeInTheDocument();

    useGameStore.setState({
      gameState: buildGameState({
        derived: {
          turn_order: [
            { player: 0, slot_index: 0, turns_from_now: 0 },
            { player: 2, slot_index: 1, turns_from_now: 1 },
            { player: 0, slot_index: 2, turns_from_now: 2 },
          ],
        },
      }),
    });
    rerender(<NextUpBadge playerId={0} />);

    expect(screen.queryByText("Next Up")).not.toBeInTheDocument();
  });

  it("renders with tooltip text for the next player only", () => {
    useGameStore.setState({
      gameState: buildGameState({
        derived: {
          turn_order: [
            { player: 0, slot_index: 0, turns_from_now: 0 },
            { player: 2, slot_index: 1, turns_from_now: 1 },
          ],
        },
      }),
    });

    render(<NextUpBadge playerId={2} />);

    expect(screen.getByTitle("This player's turn is next.")).toHaveTextContent("Next Up");
  });
});
