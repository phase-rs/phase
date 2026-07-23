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
import {
  OPPONENT_HAND_VERTICAL_SCALE,
  handFanGeometry,
  handFanVerticalMetrics,
} from "../handFanPresentation.ts";
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
    Object.defineProperty(window, "innerHeight", {
      configurable: true,
      writable: true,
      value: 768,
    });
  });

  it("uses explicit playerId instead of focusedOpponent", () => {
    render(<OpponentHand playerId={2} showCards />);

    expect(screen.getByAltText("Explicit Opponent Card")).toBeInTheDocument();
    expect(screen.queryByAltText("Focused Opponent Card")).toBeNull();
  });

  it("mirrors the shared wide, shallow hand fan geometry", () => {
    const cards = Array.from({ length: 8 }, (_, index) =>
      cardObject(100 + index, 1, `Opponent Card ${index + 1}`),
    );
    useGameStore.setState({
      gameState: buildGameState({
        players: buildPlayers([0, { id: 1, hand: cards.map((card) => card.id) }]),
        objects: buildObjectMap(...cards),
        battlefield: [],
        exile: [],
        stack: [],
        waiting_for: buildPriorityWaitingFor(),
        seat_order: [0, 1],
        eliminated_players: [],
      }),
    });

    const { container } = render(<OpponentHand playerId={1} />);
    const renderedCards = Array.from(
      container.querySelectorAll<HTMLElement>("[data-opponent-hand-card]"),
    );
    const verticalMetrics = handFanVerticalMetrics(false, OPPONENT_HAND_VERTICAL_SCALE);
    const expectedFan = handFanGeometry(
      cards.length,
      "--opponent-hand-card-w",
      verticalMetrics.arcScale,
    );

    expect(renderedCards).toHaveLength(cards.length);
    renderedCards.forEach((card, index) => {
      expect(Number(card.dataset.handRotation)).toBeCloseTo(-expectedFan.rotation(index));
      expect(Number(card.dataset.handArc)).toBeCloseTo(expectedFan.arc(index));
    });
  });

  it("scales the mirrored fan depth on compact-height screens", () => {
    Object.defineProperty(window, "innerHeight", {
      configurable: true,
      writable: true,
      value: 440,
    });
    const cards = Array.from({ length: 8 }, (_, index) =>
      cardObject(200 + index, 1, `Compact Opponent Card ${index + 1}`),
    );
    useGameStore.setState({
      gameState: buildGameState({
        players: buildPlayers([0, { id: 1, hand: cards.map((card) => card.id) }]),
        objects: buildObjectMap(...cards),
        battlefield: [],
        exile: [],
        stack: [],
        waiting_for: buildPriorityWaitingFor(),
        seat_order: [0, 1],
        eliminated_players: [],
      }),
    });

    const { container } = render(<OpponentHand playerId={1} />);
    const firstCard = container.querySelector<HTMLElement>("[data-opponent-hand-card]");

    expect(Number(firstCard?.dataset.handArc)).toBeCloseTo(
      16 * OPPONENT_HAND_VERTICAL_SCALE,
    );
  });

  // CR 701.20e (phase-rs/phase#5251): Glasses of Urza / Gitaxian Probe "look at
  // target player's hand" surfaces the looked-at cards' identities only to the
  // looking player (`private_look_player`/`private_look_ids`), distinct from
  // the public reveal sets already covered above. Before this fix, the
  // opponent-hand card thumbnail only consulted `revealed_cards` /
  // `public_revealed_cards`, so a private look never made the card visible to
  // the looker even though the engine had already sent them its real name.
  it("shows a card the engine privately looked at for this viewer, without showCards", () => {
    useGameStore.setState({
      gameState: { ...createGameState(), private_look_player: 0, private_look_ids: [22] },
    });

    render(<OpponentHand playerId={2} />);

    expect(screen.getByAltText("Explicit Opponent Card")).toBeInTheDocument();
  });

  it("does not show a privately-looked-at card to a player other than the looker", () => {
    useGameStore.setState({
      gameState: { ...createGameState(), private_look_player: 1, private_look_ids: [22] },
    });

    render(<OpponentHand playerId={2} />);

    expect(screen.queryByAltText("Explicit Opponent Card")).toBeNull();
  });
});
