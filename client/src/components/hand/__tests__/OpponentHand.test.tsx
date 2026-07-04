import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { OpponentHand } from "../OpponentHand.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: (cardName: string) => ({
    src: cardName ? `${cardName}.png` : null,
    isLoading: false,
  }),
}));

function cardObject(id: number, owner: number, name: string): GameObject {
  return {
    id,
    card_id: id,
    owner,
    controller: owner,
    zone: "Hand",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name,
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "NoCost" },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: [],
    base_power: null,
    base_toughness: null,
    base_keywords: [],
    base_color: [],
    timestamp: id,
    entered_battlefield_turn: null,
  };
}

function createGameState(): GameState {
  const focusedCard = cardObject(11, 1, "Focused Opponent Card");
  const explicitCard = cardObject(22, 2, "Explicit Opponent Card");
  return {
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [focusedCard.id], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 2, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [explicitCard.id], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    objects: {
      [focusedCard.id]: focusedCard,
      [explicitCard.id]: explicitCard,
    },
    battlefield: [],
    exile: [],
    stack: [],
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
    seat_order: [0, 1, 2],
    eliminated_players: [],
  } as unknown as GameState;
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
