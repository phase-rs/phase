import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { CombatPhaseIndicator, PhaseIndicatorLeft } from "../PhaseStopBar.tsx";

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
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

describe("PhaseStopBar", () => {
  beforeEach(() => {
    useGameStore.setState({ gameState: createGameState() });
    usePreferencesStore.setState({ phaseStops: [] });
  });

  afterEach(() => {
    cleanup();
  });

  it("describes HUD phase stops and toggles the selected stop", () => {
    render(<PhaseIndicatorLeft />);

    const mainPhase = screen.getByRole("button", {
      name: /Phase stop: First main phase\./,
    });

    expect(mainPhase).not.toHaveAttribute("title");
    expect(mainPhase).toHaveAttribute("aria-label", expect.stringContaining("Play lands and cast spells before combat."));
    expect(mainPhase).toHaveAttribute("aria-label", expect.stringContaining("No stop set: click to pause auto-pass here."));
    expect(mainPhase).toHaveAttribute("aria-label", expect.stringContaining("Current phase."));
    expect(mainPhase).toHaveAccessibleDescription(/Play lands and cast spells before combat\./);
    expect(mainPhase).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(mainPhase);

    expect(usePreferencesStore.getState().phaseStops).toEqual(["PreCombatMain"]);
    expect(mainPhase).toHaveAttribute("aria-pressed", "true");
    expect(mainPhase).toHaveAttribute("aria-label", expect.stringContaining("Stop set: click to remove this auto-pass stop."));
    expect(mainPhase).toHaveAccessibleDescription(/Stop set: click to remove this auto-pass stop\./);
  });

  it("describes combat phase group stops", () => {
    render(<CombatPhaseIndicator />);

    expect(
      screen.getByRole("button", {
        name: /Phase stop: Declare attackers step\. The attacking player chooses attackers\./,
      }),
    ).toBeInTheDocument();
  });
});
