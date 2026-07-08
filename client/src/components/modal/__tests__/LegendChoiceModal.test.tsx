import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeState(): GameState {
  const existing = buildGameObject({
    id: 10,
    card_id: 10,
    name: "Thalia, Guardian of Thraben",
    entered_battlefield_turn: 1,
    card_types: {
      supertypes: ["Legendary"],
      core_types: ["Creature"],
      subtypes: ["Human", "Soldier"],
    },
  });
  const newCopy = buildGameObject({
    id: 11,
    card_id: 11,
    name: "Thalia, Guardian of Thraben",
    entered_battlefield_turn: 2,
    card_types: {
      supertypes: ["Legendary"],
      core_types: ["Creature"],
      subtypes: ["Human", "Soldier"],
    },
  });

  return buildGameState({
    turn_number: 2,
    players: buildPlayers([0, 1]),
    priority_player: 0,
    objects: buildObjectMap(existing, newCopy),
    next_object_id: 12,
    battlefield: [10, 11],
    waiting_for: {
      type: "ChooseLegend",
      data: {
        player: 0,
        legend_name: "Thalia, Guardian of Thraben",
        candidates: [10, 11],
      },
    },
    next_timestamp: 3,
  });
}

describe("LegendChoiceModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    const state = makeState();
    useMultiplayerStore.setState({ activePlayerId: 0 });
    useGameStore.setState({
      gameMode: "online",
      gameState: state,
      waitingFor: state.waiting_for,
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("labels existing and newly entered legendary candidates", () => {
    render(<CardChoiceModal />);

    expect(screen.getByText("Already on battlefield")).toBeInTheDocument();
    expect(screen.getByText("Just entered")).toBeInTheDocument();
  });

  it("dispatches the selected legend to keep", () => {
    render(<CardChoiceModal />);

    fireEvent.click(
      screen.getByRole("button", {
        name: "Keep Thalia, Guardian of Thraben (Just entered)",
      }),
    );

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseLegend",
      data: { keep: 11 },
    });
  });
});
