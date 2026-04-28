import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { TributeModal } from "../TributeModal.tsx";
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
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 1,
    objects: {
      17: {
        id: 17,
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
        name: "Fanatic of Xenagos",
        power: 4,
        toughness: 4,
        loyalty: null,
        card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Satyr"] },
        mana_cost: { type: "NoCost" },
        keywords: [],
        abilities: [],
        trigger_definitions: [],
        replacement_definitions: [],
        static_definitions: [],
        color: [],
        base_power: 4,
        base_toughness: 4,
        base_keywords: [],
        base_color: [],
        timestamp: 1,
        entered_battlefield_turn: 1,
      },
    },
    next_object_id: 100,
    battlefield: [17],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: {
      type: "TributeChoice",
      data: { player: 1, source_id: 17, count: 3 },
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

describe("TributeModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
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
