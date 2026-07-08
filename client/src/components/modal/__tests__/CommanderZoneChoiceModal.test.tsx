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
  const commander = buildGameObject({
    id: 10,
    card_id: 10,
    name: "Witherbloom, the Balancer",
    card_types: {
      supertypes: ["Legendary"],
      core_types: ["Creature"],
      subtypes: ["Elder", "Dragon"],
    },
  });

  return buildGameState({
    players: buildPlayers([0, 1]),
    priority_player: 0,
    objects: buildObjectMap(commander),
    next_object_id: 11,
    waiting_for: {
      type: "CommanderZoneChoice",
      data: {
        player: 0,
        commander_id: 10,
        current_zone: "graveyard",
      },
    },
  });
}

describe("CommanderZoneChoiceModal", () => {
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

  it("uses a compact command-zone-specific shell", () => {
    render(<CardChoiceModal />);

    expect(screen.getByTestId("commander-zone-choice-dialog")).toHaveClass("max-w-[34rem]");
    expect(screen.getByText("Witherbloom, the Balancer was put into the Graveyard. Return to the Command Zone?")).toBeInTheDocument();
  });

  it("uses distinct action tones and dispatches the command-zone choice", () => {
    render(<CardChoiceModal />);

    const commandZone = screen.getByRole("button", { name: "Command Zone" });
    const leave = screen.getByRole("button", { name: "Leave in Graveyard" });

    expect(commandZone).toHaveClass("text-cyan-100");
    expect(leave).toHaveClass("text-amber-100");

    fireEvent.click(commandZone);
    expect(dispatchMock).toHaveBeenLastCalledWith({
      type: "DecideOptionalEffect",
      data: { accept: true },
    });

    fireEvent.click(leave);
    expect(dispatchMock).toHaveBeenLastCalledWith({
      type: "DecideOptionalEffect",
      data: { accept: false },
    });
  });
});
