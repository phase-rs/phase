import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, WaitingFor } from "../../../adapter/types.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeHandCard(id: number, name: string) {
  return {
    id,
    card_id: id,
    owner: 0,
    controller: 0,
    zone: "Hand" as const,
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
    card_types: { supertypes: [], core_types: ["Sorcery"], subtypes: [] },
    mana_cost: { type: "NoCost" as const },
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
    timestamp: 1,
    entered_battlefield_turn: 1,
  };
}

const learnChoice: WaitingFor = {
  type: "LearnChoice",
  data: { player: 0, hand_cards: [42, 43] },
};

function makeState(): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [42, 43], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 0,
    objects: {
      42: makeHandCard(42, "Lightning Bolt"),
      43: makeHandCard(43, "Counterspell"),
    },
    next_object_id: 100,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: learnChoice,
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 2,
    eliminated_players: [],
  } as unknown as GameState;
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
