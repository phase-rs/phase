import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, WaitingFor } from "../../../adapter/types.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeHandCard(id: number, name: string) {
  return buildGameObjectWithCoreTypes(["Sorcery"], {
    id,
    card_id: id,
    zone: "Hand",
    name,
    timestamp: 1,
    entered_battlefield_turn: 1,
  });
}

const learnChoice: WaitingFor = {
  type: "LearnChoice",
  data: { player: 0, hand_cards: [42, 43] },
};

function makeState(): GameState {
  return buildGameState({
    players: buildPlayers([{ id: 0, hand: [42, 43] }, 1]),
    objects: buildObjectMap(
      makeHandCard(42, "Lightning Bolt"),
      makeHandCard(43, "Counterspell"),
    ),
    next_object_id: 100,
    waiting_for: learnChoice,
    next_timestamp: 2,
  });
}

describe("LearnModal (via CardChoiceModal)", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
    useGameStore.setState({
      gameMode: "online",
      gameState: makeState(),
      waitingFor: learnChoice,
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders all hand cards offered for the learn rummage", () => {
    render(<CardChoiceModal />);

    expect(screen.getByRole("button", { name: "Lightning Bolt" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Counterspell" })).toBeInTheDocument();
  });

  it("keeps Discard & draw disabled until a card is selected, then dispatches Rummage", () => {
    render(<CardChoiceModal />);

    expect(screen.getByRole("button", { name: "Discard & draw" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Counterspell" }));
    expect(screen.getByRole("button", { name: "Discard & draw" })).toBeEnabled();

    fireEvent.click(screen.getByRole("button", { name: "Discard & draw" }));

    expect(dispatchMock).toHaveBeenCalledTimes(1);
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "LearnDecision",
      data: { choice: { type: "Rummage", data: { card_id: 43 } } },
    });
  });

  it("dispatches Skip when the player declines without selecting", () => {
    render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: "Skip" }));

    expect(dispatchMock).toHaveBeenCalledTimes(1);
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "LearnDecision",
      data: { choice: { type: "Skip" } },
    });
  });

  it("is registered as a handled waiting-for state (suppresses the orphan safety-net)", () => {
    expect(isWaitingForHandled(learnChoice)).toBe(true);
  });
});
