import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeCreature(id: number, name: string) {
  return buildGameObjectWithCoreTypes(["Creature"], {
    id,
    card_id: id,
    zone: "Battlefield",
    name,
    power: 1,
    toughness: 1,
    base_power: 1,
    base_toughness: 1,
    timestamp: 1,
    entered_battlefield_turn: 1,
  });
}

function makeState(): GameState {
  const creatures = [
    makeCreature(42, "Frodo Baggins"),
    makeCreature(43, "Samwise Gamgee"),
  ];
  return buildGameState({
    phase: "DeclareAttackers",
    players: buildPlayers([{ id: 0, life: 40 }, { id: 1, life: 40 }]),
    objects: buildObjectMap(...creatures),
    next_object_id: 100,
    battlefield: creatures.map((creature) => creature.id),
    waiting_for: {
      type: "ChooseRingBearer",
      data: { player: 0, candidates: [42, 43] },
    },
    next_timestamp: 2,
  });
}

describe("ChooseRingBearer board choice", () => {
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

  it("suppresses the modal for board-native ring-bearer selection", () => {
    render(<CardChoiceModal />);

    expect(screen.queryByRole("button")).toBeNull();
    expect(dispatchMock).not.toHaveBeenCalled();
  });
});
