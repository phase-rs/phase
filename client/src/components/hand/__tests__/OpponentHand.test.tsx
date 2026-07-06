import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import {
  buildGameObjectWithCoreTypes,
  buildObjectMap,
} from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { OpponentHand } from "../OpponentHand.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: (cardName: string) => ({
    src: cardName ? `${cardName}.png` : null,
    isLoading: false,
  }),
}));

function cardObject(id: number, owner: number, name: string) {
  return buildGameObjectWithCoreTypes(["Creature"], {
    id,
    card_id: id,
    owner,
    controller: owner,
    zone: "Hand",
    name,
    timestamp: id,
    entered_battlefield_turn: null,
  });
}

function createGameState() {
  const focusedCard = cardObject(11, 1, "Focused Opponent Card");
  const explicitCard = cardObject(22, 2, "Explicit Opponent Card");
  return buildGameState({
    players: buildPlayers([
      0,
      { id: 1, hand: [focusedCard.id] },
      { id: 2, hand: [explicitCard.id] },
    ]),
    objects: buildObjectMap(focusedCard, explicitCard),
    battlefield: [],
    exile: [],
    stack: [],
    waiting_for: buildPriorityWaitingFor(),
    seat_order: [0, 1, 2],
    eliminated_players: [],
  });
}

describe("OpponentHand", () => {
  beforeEach(() => {
    useGameStore.setState({
      gameMode: "local",
      gameState: createGameState(),
    });
    useUiStore.setState({ focusedOpponent: 1 });
  });

  afterEach(() => {
    cleanup();
  });

  it("uses explicit playerId instead of focusedOpponent", () => {
    render(<OpponentHand playerId={2} showCards />);

    expect(screen.getByAltText("Explicit Opponent Card")).toBeInTheDocument();
    expect(screen.queryByAltText("Focused Opponent Card")).toBeNull();
  });
});
