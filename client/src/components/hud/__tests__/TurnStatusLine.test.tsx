/**
 * Runtime tests for TurnStatusLine — the persistent "who has priority / why"
 * narration. Renders the real component against real i18n + Zustand stores and
 * asserts on the user-visible English copy, so framing bugs (e.g. "Your
 * priority" shown to a spectator) are caught at the rendered-text level.
 */
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameState, Phase, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { TurnStatusLine } from "../TurnStatusLine.tsx";

function createGameState(o: { active_player?: number; phase?: Phase; stack?: GameState["stack"] } = {}): GameState {
  return {
    turn_number: 1,
    active_player: o.active_player ?? 0,
    phase: o.phase ?? "PreCombatMain",
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: o.active_player ?? 0,
    objects: {},
    next_object_id: 100,
    battlefield: [],
    stack: o.stack ?? [],
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
    turn_decision_controller: o.active_player ?? 0,
    format_config: {
      format: "Standard", starting_life: 20, min_players: 2, max_players: 2, deck_size: 60,
      singleton: false, command_zone: false, commander_damage_threshold: null,
      range_of_influence: null, team_based: false, uses_commander: false, allow_debug_actions: false,
    },
    eliminated_players: [],
  };
}

const ONE_STACK_ENTRY = [{ object_id: 50 }] as unknown as GameState["stack"];

function setup(opts: { seat: number; waitingFor: WaitingFor; state?: Parameters<typeof createGameState>[0]; spectate?: boolean }) {
  useGameStore.setState({
    gameMode: opts.spectate ? "spectate" : "online",
    gameState: createGameState(opts.state),
    waitingFor: opts.waitingFor,
  });
  useMultiplayerStore.setState({
    activePlayerId: opts.seat,
    isSpectator: opts.spectate ?? false,
    playerNames: new Map([[1, "Sorin"]]),
  });
}

describe("TurnStatusLine", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false, playerNames: new Map() });
  });
  afterEach(() => {
    cleanup();
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false, playerNames: new Map() });
  });

  it("announces the local player's own priority with the phase reason", () => {
    setup({ seat: 0, waitingFor: { type: "Priority", data: { player: 0 } }, state: { active_player: 0 } });
    render(<TurnStatusLine />);
    const region = screen.getByRole("status");
    expect(region).toHaveTextContent("Your priority — main phase");
    expect(region).toHaveAttribute("aria-live", "polite");
  });

  it("names the opponent we are waiting on, with the stack reason", () => {
    setup({
      seat: 0,
      waitingFor: { type: "Priority", data: { player: 1 } },
      state: { active_player: 0, stack: ONE_STACK_ENTRY },
    });
    render(<TurnStatusLine />);
    expect(screen.getByRole("status")).toHaveTextContent("Waiting for Sorin — responding to the stack");
  });

  it("never frames the decision as the spectator's own", () => {
    setup({ seat: 0, waitingFor: { type: "Priority", data: { player: 0 } }, spectate: true });
    render(<TurnStatusLine />);
    const region = screen.getByRole("status");
    expect(region).not.toHaveTextContent("Your priority");
    expect(region).toHaveTextContent(/Waiting for/);
  });

  it("renders nothing when no decision is pending", () => {
    setup({ seat: 0, waitingFor: { type: "GameOver", data: { winner: 0 } } });
    render(<TurnStatusLine />);
    expect(screen.queryByRole("status")).toBeNull();
  });
});
