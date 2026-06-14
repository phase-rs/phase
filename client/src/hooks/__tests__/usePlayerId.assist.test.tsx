/**
 * Runtime test for the CR 702.132a Assist acting-player routing fix in
 * `waitingPlayer`. Under an `AssistPayment` waiting state the CHOSEN helper is
 * the actor (the prompt carries `caster`/`chosen`, no `player` field). These
 * tests drive `useCanActForWaitingState` through that branch: it must be true
 * for the `chosen` seat and false for the `caster` seat. Reverting the
 * `AssistPayment` branch makes `waitingPlayer` return null (no `player` field),
 * which flips the `chosen`-seat assertion to false.
 */
import { renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameState, WaitingFor } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { useCanActForWaitingState } from "../usePlayerId";

function createGameState(): GameState {
  return {
    turn_number: 1,
    active_player: 1,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 1,
    objects: {},
    next_object_id: 100,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: { type: "ManaPayment", data: { player: 0 } },
    has_pending_cast: true,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
    seat_order: [0, 1],
    turn_decision_controller: 1,
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
      uses_commander: false,
      allow_debug_actions: false,
    },
    eliminated_players: [],
  };
}

// caster = 1, chosen = 0: the helper (seat 0) is the one who must act.
const ASSIST_PAYMENT: WaitingFor = {
  type: "AssistPayment",
  data: { caster: 1, chosen: 0, max_generic: 3 },
};

function setLocalSeat(seat: number) {
  useGameStore.setState({
    gameMode: "online",
    gameState: createGameState(),
    waitingFor: ASSIST_PAYMENT,
  });
  useMultiplayerStore.setState({ activePlayerId: seat, isSpectator: false });
}

describe("useCanActForWaitingState — Assist payment routing (CR 702.132a)", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
  });

  afterEach(() => {
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
  });

  it("is true for the chosen helper seat", () => {
    setLocalSeat(0); // chosen
    const { result } = renderHook(() => useCanActForWaitingState());
    expect(result.current).toBe(true);
  });

  it("is false for the caster seat", () => {
    setLocalSeat(1); // caster, not the helper
    const { result } = renderHook(() => useCanActForWaitingState());
    expect(result.current).toBe(false);
  });
});
