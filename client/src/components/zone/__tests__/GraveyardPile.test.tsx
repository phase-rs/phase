import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory.ts";
import { GraveyardPile } from "../GraveyardPile.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: "card.png", isLoading: false }),
}));

const FIRST = 101;
const TOP = 102;

function setGraveyard(graveyard: number[]) {
  const first = buildGameObject({ id: FIRST, name: "First Card", zone: "Graveyard", entered_battlefield_turn: null });
  const top = buildGameObject({ id: TOP, name: "Top Card", zone: "Graveyard", entered_battlefield_turn: null });
  const gameState = buildGameState({
    players: buildPlayers([{ id: 0, graveyard }, 1]),
    objects: buildObjectMap(first, top),
    battlefield: [],
    exile: [],
    stack: [],
    waiting_for: buildPriorityWaitingFor(),
  });
  useGameStore.setState({ gameState, waitingFor: gameState.waiting_for, legalActionsByObject: {} });
}

function renderPile() {
  const { container } = render(<GraveyardPile playerId={0} onClick={vi.fn()} />);
  return container.querySelector<HTMLElement>('[data-graveyard-pile="0"]');
}

beforeEach(() => {
  useAnimationStore.getState().clearQueue();
});

afterEach(() => {
  cleanup();
  useAnimationStore.getState().clearQueue();
  useGameStore.setState({ gameState: null, waitingFor: null, legalActionsByObject: {} });
});

describe("GraveyardPile flight veil", () => {
  it("shows the card beneath a top card that is still in flight, keeping the count and anchor", () => {
    setGraveyard([FIRST, TOP]);
    useAnimationStore.getState().veilFlight(TOP);

    const pile = renderPile();

    expect(screen.getByAltText("First Card")).toBeInTheDocument();
    expect(screen.queryByAltText("Top Card")).toBeNull();
    expect(pile).toHaveTextContent("2");
    expect(pile).toHaveAttribute("data-grouped-ids", `${FIRST} ${TOP}`);
  });

  it("shows an empty frame when every card is in flight, keeping the pile and count", () => {
    setGraveyard([FIRST, TOP]);
    useAnimationStore.getState().veilFlight(FIRST);
    useAnimationStore.getState().veilFlight(TOP);

    const pile = renderPile();

    expect(pile).not.toBeNull();
    expect(screen.queryByRole("img")).toBeNull();
    expect(pile).toHaveTextContent("2");
  });

  it("keeps a one-card pile rendered while its only card is in flight", () => {
    setGraveyard([TOP]);
    useAnimationStore.getState().veilFlight(TOP);

    const pile = renderPile();

    expect(pile).not.toBeNull();
    expect(screen.queryByRole("img")).toBeNull();
    expect(pile).toHaveTextContent("1");
  });

  it("ignores a flight veil on a card below the top", () => {
    setGraveyard([FIRST, TOP]);
    useAnimationStore.getState().veilFlight(FIRST);

    renderPile();

    expect(screen.getByAltText("Top Card")).toBeInTheDocument();
  });

  it("shows the top card once its flight releases it", () => {
    setGraveyard([FIRST, TOP]);
    useAnimationStore.getState().veilFlight(TOP);
    renderPile();

    act(() => useAnimationStore.getState().unveilFlight(TOP));

    expect(screen.getByAltText("Top Card")).toBeInTheDocument();
  });
});
