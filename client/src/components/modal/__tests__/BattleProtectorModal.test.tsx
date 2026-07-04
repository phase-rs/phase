import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { BattleProtectorModal } from "../BattleProtectorModal.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeState(): GameState {
  const battle = buildGameObjectWithCoreTypes(["Battle"], {
    id: 42,
    card_id: 1,
    zone: "Battlefield",
    name: "Invasion of Arcavios",
    card_types: { supertypes: [], core_types: ["Battle"], subtypes: ["Siege"] },
    timestamp: 1,
    entered_battlefield_turn: 1,
  });
  return buildGameState({
    players: buildPlayers([{ id: 0, life: 40 }, { id: 1, life: 40 }, { id: 2, life: 40 }]),
    objects: buildObjectMap(battle),
    next_object_id: 100,
    battlefield: [42],
    waiting_for: {
      type: "BattleProtectorChoice",
      data: { player: 0, battle_id: 42, candidates: [1, 2] },
    },
    next_timestamp: 2,
  });
}

describe("BattleProtectorModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
    const state = makeState();
    useGameStore.setState({
      gameMode: "online",
      gameState: state,
      waitingFor: state.waiting_for,
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders candidates and dispatches ChooseBattleProtector on confirm", () => {
    render(<BattleProtectorModal />);

    expect(screen.getByText(/Choose a Protector/)).toBeInTheDocument();
    expect(screen.getByText(/Invasion of Arcavios/)).toBeInTheDocument();

    // Both candidates rendered
    const player2 = screen.getByRole("button", { name: "Opp 2" });
    const player3 = screen.getByRole("button", { name: "Opp 3" });
    expect(player2).toBeInTheDocument();
    expect(player3).toBeInTheDocument();

    // Confirm is disabled until a candidate is picked
    const confirm = screen.getByRole("button", { name: "Confirm" });
    expect(confirm).toBeDisabled();

    fireEvent.click(player3);
    expect(confirm).not.toBeDisabled();

    fireEvent.click(confirm);
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseBattleProtector",
      data: { protector: 2 },
    });
  });

  it("renders nothing when the current player cannot act", () => {
    useMultiplayerStore.setState({ activePlayerId: 1 });
    const { container } = render(<BattleProtectorModal />);
    expect(container).toBeEmptyDOMElement();
  });
});
