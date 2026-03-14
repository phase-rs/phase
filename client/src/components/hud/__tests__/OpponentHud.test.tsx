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
      { id: 0, life: 40, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0 },
      { id: 1, life: 40, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0 },
      { id: 2, life: 40, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0 },
      { id: 3, life: 40, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0 },
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
});
