import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { BattleProtectorModal } from "../BattleProtectorModal.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeState(): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 2, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 0,
    objects: {
      42: {
        id: 42,
        card_id: 1,
        owner: 0,
        controller: 0,
        zone: "Battlefield",
        tapped: false,
        face_down: false,
        flipped: false,
        transformed: false,
        damage_marked: 0,
        dealt_deathtouch_damage: false,
        attached_to: null,
        attachments: [],
        counters: {},
        name: "Invasion of Arcavios",
        power: null,
        toughness: null,
        loyalty: null,
        card_types: { supertypes: [], core_types: ["Battle"], subtypes: ["Siege"] },
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
        timestamp: 1,
        entered_battlefield_turn: 1,
      },
    },
    next_object_id: 100,
    battlefield: [42],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: {
      type: "BattleProtectorChoice",
      data: { player: 0, battle_id: 42, candidates: [1, 2] },
    },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 2,
    eliminated_players: [],
  };
}

describe("BattleProtectorModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
    const state = makeState();
    useGameStore.setState({
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
    const player2 = screen.getByRole("button", { name: "Player 2" });
    const player3 = screen.getByRole("button", { name: "Player 3" });
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
