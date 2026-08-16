import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { TributeModal } from "../TributeModal.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeState(): GameState {
  const creature = buildGameObjectWithCoreTypes(["Creature"], {
    id: 17,
    card_id: 1,
    zone: "Battlefield",
    name: "Fanatic of Xenagos",
    power: 4,
    toughness: 4,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Satyr"] },
    base_power: 4,
    base_toughness: 4,
    timestamp: 1,
    entered_battlefield_turn: 1,
  });
  return buildGameState({
    priority_player: 1,
    objects: buildObjectMap(creature),
    next_object_id: 100,
    battlefield: [17],
    waiting_for: {
      type: "TributeChoice",
      data: { player: 1, source_id: 17, count: 3 },
    },
    next_timestamp: 2,
  });
}

describe("TributeModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useGameStore.setState({
      gameMode: "online",
    });
    useMultiplayerStore.setState({ activePlayerId: 1 });
    const state = makeState();
    useGameStore.setState({
      gameState: state,
      waitingFor: state.waiting_for,
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("dispatches accept=true when the chosen opponent pays tribute", () => {
    render(<TributeModal />);

    // Title is "Tribute — Fanatic of Xenagos"
    expect(screen.getByText(/Tribute.*Fanatic of Xenagos/)).toBeInTheDocument();
    // The count is referenced in the subtitle and in the Pay button description
    expect(screen.getAllByText(/3 \+1\/\+1 counters/).length).toBeGreaterThan(0);

    fireEvent.click(screen.getByText("Pay Tribute"));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DecideOptionalEffect",
      data: { accept: true },
    });
  });

  it("dispatches accept=false when the chosen opponent declines", () => {
    render(<TributeModal />);

    fireEvent.click(screen.getByText("Decline"));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DecideOptionalEffect",
      data: { accept: false },
    });
  });

  it("renders nothing when the current player is not the chosen opponent", () => {
    useMultiplayerStore.setState({ activePlayerId: 0 });
    const { container } = render(<TributeModal />);
    expect(container).toBeEmptyDOMElement();
  });
});
