import { act } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { OpponentHud } from "../OpponentHud.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return {
    turn_number: 1,
    active_player: 2,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0 },
      { id: 1, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0 },
      { id: 2, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0 },
      { id: 3, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0 },
    ],
    priority_player: 2,
    objects: {},
    next_object_id: 1,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: { type: "Priority", data: { player: 2 } },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
    seat_order: [0, 1, 2, 3],
    format_config: {
      format: "Commander",
      starting_life: 40,
      min_players: 2,
      max_players: 4,
      deck_size: 100,
      singleton: true,
      command_zone: true,
      commander_damage_threshold: 21,
      range_of_influence: null,
      team_based: false,
    },
    eliminated_players: [],
    ...overrides,
  };
}

describe("OpponentHud", () => {
  beforeEach(() => {
    localStorage.clear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
    usePreferencesStore.setState({ followActiveOpponent: false });
    useUiStore.setState({ focusedOpponent: 1 });
    useGameStore.setState({ gameState: createGameState() });
  });

  afterEach(() => {
    cleanup();
  });

  it("auto-selects the active opponent when Follow is enabled", async () => {
    render(<OpponentHud />);

    fireEvent.click(screen.getByRole("button", { name: "Follow" }));

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(2);
    });

    act(() => {
      useGameStore.setState({
        gameState: createGameState({ active_player: 3, priority_player: 3, waiting_for: { type: "Priority", data: { player: 3 } } }),
      });
    });

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(3);
    });
  });

  it("does not override manual selection while Follow is disabled", async () => {
    render(<OpponentHud />);

    fireEvent.click(screen.getByRole("button", { name: /Opp 4/ }));

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(3);
    });

    act(() => {
      useGameStore.setState({
        gameState: createGameState({ active_player: 2, priority_player: 2, waiting_for: { type: "Priority", data: { player: 2 } } }),
      });
    });

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(3);
    });
  });

  it("renders compact poison and speed badges in multiplayer tabs", () => {
    const gameState = createGameState();
    gameState.players[1].poison_counters = 3;
    gameState.players[1].speed = 2;

    act(() => {
      useGameStore.setState({ gameState });
    });

    render(<OpponentHud />);

    expect(screen.getByTitle("Poison counters: 3")).toHaveAttribute("aria-label", "3 poison counters");
    expect(screen.getByTitle("Speed: 2")).toHaveAttribute("aria-label", "Speed 2");
    expect(screen.queryByText("Speed")).toBeNull();
  });

  it("hides zero poison counters", () => {
    render(<OpponentHud />);

    expect(screen.queryByTitle(/Poison counters:/)).toBeNull();
  });

  it("renders compact poison and speed badges for the 1v1 opponent HUD", () => {
    const gameState = createGameState({
      players: [
        { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0 },
        { id: 1, life: 20, poison_counters: 4, speed: 1, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0 },
      ],
      active_player: 1,
      priority_player: 1,
      waiting_for: { type: "Priority", data: { player: 1 } },
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
    });

    act(() => {
      useGameStore.setState({ gameState });
    });

    render(<OpponentHud />);

    expect(screen.getByTitle("Poison counters: 4")).toHaveAttribute("aria-label", "4 poison counters");
    expect(screen.getByTitle("Speed: 1")).toHaveAttribute("aria-label", "Speed 1");
    expect(screen.queryByText("Speed")).toBeNull();
  });
});
