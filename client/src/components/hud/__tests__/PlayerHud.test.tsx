import { act } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { PlayerHud } from "../PlayerHud.tsx";

describe("PlayerHud", () => {
  beforeEach(() => {
    useMultiplayerStore.setState({ activePlayerId: 0 });
    useGameStore.setState({ gameState: buildGameState() });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders local poison and speed as compact accessible badges", () => {
    const gameState = buildGameState();
    gameState.players[0].poison_counters = 8;
    gameState.players[0].speed = 3;

    act(() => {
      useGameStore.setState({ gameState });
    });

    render(<PlayerHud />);

    // Badges now use the custom GameplayTooltip (text rendered in the DOM)
    // rather than a native `title`; the aria-label stays on the badge element.
    expect(screen.getByLabelText("8 poison counters")).toBeInTheDocument();
    expect(screen.getByText("Poison counters: 8")).toBeInTheDocument();
    expect(screen.getByLabelText("Speed 3")).toBeInTheDocument();
    expect(screen.getByText("Speed: 3")).toBeInTheDocument();
    expect(screen.queryByText("Speed")).toBeNull();
  });

  it("hides local zero poison counters", () => {
    render(<PlayerHud />);

    expect(screen.queryByText(/Poison counters:/)).toBeNull();
  });

  it("renders local Next Up badge only for the next actual turn", () => {
    act(() => {
      useGameStore.setState({
        gameState: buildGameState({
          derived: {
            turn_order: [
              { player: 0, slot_index: 1, turns_from_now: 1 },
              { player: 0, slot_index: 2, turns_from_now: 2 },
            ],
          },
        }),
      });
    });

    render(<PlayerHud />);

    expect(screen.getByTitle("This player's turn is next.")).toHaveTextContent("Next Up");
  });
});
