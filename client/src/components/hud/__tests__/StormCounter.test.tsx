import { act } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { PlayerHud } from "../PlayerHud.tsx";
import { StormCounter } from "../StormCounter.tsx";

describe("StormCounter", () => {
  afterEach(() => {
    cleanup();
  });

  it("renders the engine-provided table-wide Storm copy count", () => {
    render(<StormCounter count={3} />);

    expect(
      screen.getByRole("status", { name: "Storm count: 3 copies" }),
    ).toHaveTextContent("3");
  });

  it("stays hidden when Storm would create no copies", () => {
    render(<StormCounter count={0} />);

    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("renders the shared counter once in the player HUD", () => {
    act(() => {
      useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false });
      useGameStore.setState({
        gameState: buildGameState({ derived: { storm_count: 3 } }),
        gameMode: null,
        waitingFor: null,
      });
    });

    render(<PlayerHud />);

    expect(
      screen.getAllByRole("status", { name: "Storm count: 3 copies" }),
    ).toHaveLength(1);
  });
});
