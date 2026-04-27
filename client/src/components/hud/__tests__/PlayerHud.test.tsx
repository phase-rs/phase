import { act } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { PlayerHud } from "../PlayerHud.tsx";

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0 },
    ],
    priority_player: 0,
    objects: {},
    next_object_id: 1,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
    seat_order: [0, 1],
    format_config: {
      format: "Standard",
      starting_life: 20,
      min_players: 2,
      max_players: 2,
      deck_size: 60,
      singleton: false,
      command_zone: false,
      commander_damage_threshold: null,
      range_of_influence: null,
      team_based: false,
    },
    eliminated_players: [],
    ...overrides,
  };
}

describe("PlayerHud", () => {
  beforeEach(() => {
    useMultiplayerStore.setState({ activePlayerId: 0 });
    useGameStore.setState({ gameState: createGameState() });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders local poison and speed as compact accessible badges", () => {
    const gameState = createGameState();
    gameState.players[0].poison_counters = 8;
    gameState.players[0].speed = 3;

    act(() => {
      useGameStore.setState({ gameState });
    });

    render(<PlayerHud />);

    expect(screen.getByTitle("Poison counters: 8")).toHaveAttribute("aria-label", "8 poison counters");
    expect(screen.getByTitle("Speed: 3")).toHaveAttribute("aria-label", "Speed 3");
    expect(screen.queryByText("Speed")).toBeNull();
  });

  it("hides local zero poison counters", () => {
    render(<PlayerHud />);

    expect(screen.queryByTitle(/Poison counters:/)).toBeNull();
  });
});
