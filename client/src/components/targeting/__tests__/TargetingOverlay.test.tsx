import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type { GameState } from "../../../adapter/types.ts";
import { TargetingOverlay } from "../TargetingOverlay.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";

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
    waiting_for: {
      type: "TriggerTargetSelection",
      data: {
        player: 0,
        target_slots: [{ legal_targets: [{ Player: 1 }], optional: false }],
        selection: {
          current_slot: 0,
          current_legal_targets: [{ Player: 1 }],
        },
      },
    },
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

describe("TargetingOverlay", () => {
  beforeEach(() => {
    act(() => {
      useMultiplayerStore.setState({ activePlayerId: 0 });
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("does not render player target buttons (handled by HUD components)", () => {
    const dispatch = vi.fn().mockResolvedValue([]);

    act(() => {
      useGameStore.setState({
        gameState: createGameState(),
        waitingFor: createGameState().waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    expect(screen.queryByRole("button", { name: /Target Player/i })).toBeNull();
    expect(screen.getByText("Choose a target")).toBeInTheDocument();
  });

  it("dispatches null target when the active engine slot is optional and skipped", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState({
      waiting_for: {
        type: "TargetSelection",
        data: {
          player: 0,
          pending_cast: {
            object_id: 5,
            card_id: 10,
            ability: { targets: [] },
            cost: { type: "NoCost" },
          },
          target_slots: [{ legal_targets: [], optional: true }],
          selection: {
            current_slot: 0,
            current_legal_targets: [],
          },
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    fireEvent.click(screen.getByRole("button", { name: "Skip" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: null },
    });
  });
});
