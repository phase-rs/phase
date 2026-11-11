import { act } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { PoisonLossView } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { PoisonCounterBadge } from "../HudBadges.tsx";

/** Seeds `derived.poison_loss` for player 1 (or nothing) and renders the badge. */
function renderBadge(value: number, lossRule?: PoisonLossView) {
  act(() => {
    useGameStore.setState({
      gameState: buildGameState({
        derived: lossRule ? { poison_loss: { "1": lossRule } } : {},
      }),
    });
  });
  render(<PoisonCounterBadge playerId={1} value={value} />);
  return screen.getByRole("img", { name: `${value} poison counters` });
}

describe("PoisonCounterBadge", () => {
  afterEach(() => {
    cleanup();
    act(() => {
      useGameStore.setState({ gameState: null });
    });
  });

  it("states the engine's house-rule individual threshold", () => {
    renderBadge(10, { scope: "Player", threshold: 15, near_lethal: false });

    expect(screen.getByText("Poison counters: 10")).toBeInTheDocument();
    expect(
      screen.getByText("A player with 15 or more poison counters loses the game."),
    ).toBeInTheDocument();
  });

  it("states the CR default threshold when the engine measures ten", () => {
    renderBadge(10, { scope: "Player", threshold: 10, near_lethal: true });

    expect(
      screen.getByText("A player with 10 or more poison counters loses the game."),
    ).toBeInTheDocument();
  });

  it("states the team rule for shared poison", () => {
    renderBadge(10, { scope: "Team", threshold: 15, near_lethal: false });

    expect(
      screen.getByText("A team with 15 or more poison counters loses the game."),
    ).toBeInTheDocument();
    expect(screen.queryByText(/A player with/)).toBeNull();
  });

  it("glows from the engine's near-lethal flag, not the counter value", () => {
    const nearLethal = renderBadge(3, { scope: "Player", threshold: 5, near_lethal: true });
    expect(nearLethal).toHaveClass("bg-lime-300");
    cleanup();

    const notNearLethal = renderBadge(9, { scope: "Player", threshold: 15, near_lethal: false });
    expect(notNearLethal).toHaveClass("bg-lime-400");
    expect(notNearLethal).not.toHaveClass("bg-lime-300");
  });

  it("shows only the count when the engine reports no loss rule", () => {
    renderBadge(10);

    expect(screen.getByText("Poison counters: 10")).toBeInTheDocument();
    expect(screen.queryByText(/loses the game/)).toBeNull();
  });
});
