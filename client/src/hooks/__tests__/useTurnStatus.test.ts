/**
 * Runtime tests for `useTurnStatus` — the single source for "whose turn / who
 * must act / why" presentation. Drives the real Zustand stores (no mocks) so
 * the composed authorities (`waitingPlayer`, `useCanActForWaitingState`,
 * `usePlayerId`) run exactly as in production.
 *
 * The load-bearing assertions are the SEPARATION cases: turn ownership
 * (`isMyTurn`, raw seat) must stay independent of decision authority
 * (`canIActNow`), so an opponent holding priority during my turn, and a
 * turn-control (Mindslaver) seat, both report correctly.
 */
import { renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameState, Phase, WaitingFor } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { useTurnStatus } from "../useTurnStatus";

interface Overrides {
  active_player?: number;
  turn_decision_controller?: number;
  phase?: Phase;
  stack?: GameState["stack"];
}

function createGameState(o: Overrides = {}): GameState {
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
    turn_decision_controller: o.turn_decision_controller ?? (o.active_player ?? 0),
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

/** A non-empty stack: one entry is enough for the `Priority` sub-branch. */
const ONE_STACK_ENTRY = [{ object_id: 50 }] as unknown as GameState["stack"];

function setup(opts: {
  seat: number;
  waitingFor: WaitingFor;
  state?: Overrides;
  spectate?: boolean;
}) {
  useGameStore.setState({
    gameMode: opts.spectate ? "spectate" : "online",
    gameState: createGameState(opts.state),
    waitingFor: opts.waitingFor,
  });
  useMultiplayerStore.setState({
    activePlayerId: opts.seat,
    isSpectator: opts.spectate ?? false,
  });
}

describe("useTurnStatus", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
  });
  afterEach(() => {
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
  });

  it("my priority on an empty main-phase stack: actionable, reason is the main-phase window", () => {
    setup({ seat: 0, waitingFor: { type: "Priority", data: { player: 0 } }, state: { active_player: 0 } });
    const { result } = renderHook(() => useTurnStatus());
    expect(result.current.isMyTurn).toBe(true);
    expect(result.current.canIActNow).toBe(true);
    expect(result.current.waitingSeatId).toBe(0);
    expect(result.current.waitingOnOpponent).toBe(false);
    expect(result.current.reason?.key).toBe("status.reason.priorityMain");
  });

  it("separates turn from priority: opponent holds priority during MY turn", () => {
    // active_player = 0 (my turn), but seat 1 holds the priority window with a
    // spell on the stack — I am waiting on them even though it is my turn.
    setup({
      seat: 0,
      waitingFor: { type: "Priority", data: { player: 1 } },
      state: { active_player: 0, stack: ONE_STACK_ENTRY },
    });
    const { result } = renderHook(() => useTurnStatus());
    expect(result.current.isMyTurn).toBe(true); // still my turn
    expect(result.current.canIActNow).toBe(false); // but not my decision
    expect(result.current.waitingSeatId).toBe(1);
    expect(result.current.waitingOnOpponent).toBe(true);
    expect(result.current.reason?.key).toBe("status.reason.respondingToStack");
  });

  it("turn control (Mindslaver): isMyTurn is false but I am the authorized actor", () => {
    // active_player = 1 (the victim's turn); I (seat 0) am the controller.
    setup({
      seat: 0,
      waitingFor: { type: "Priority", data: { player: 1 } },
      state: { active_player: 1, turn_decision_controller: 0 },
    });
    const { result } = renderHook(() => useTurnStatus());
    expect(result.current.isMyTurn).toBe(false); // not my turn (raw seat)
    expect(result.current.canIActNow).toBe(true); // but I act for it
  });

  it("spectator: never frames a decision as the viewer's own", () => {
    setup({ seat: 0, waitingFor: { type: "Priority", data: { player: 0 } }, spectate: true });
    const { result } = renderHook(() => useTurnStatus());
    expect(result.current.isMyTurn).toBe(false);
    expect(result.current.canIActNow).toBe(false);
    expect(result.current.waitingOnOpponent).toBe(false);
    // The seat is still resolved so the status line can name it neutrally.
    expect(result.current.waitingSeatId).toBe(0);
  });

  it("game over: nothing pending to narrate", () => {
    setup({ seat: 0, waitingFor: { type: "GameOver", data: { winner: 0 } } });
    const { result } = renderHook(() => useTurnStatus());
    expect(result.current.waitingSeatId).toBeNull();
    expect(result.current.reason).toBeNull();
  });

  it("resolves the semantic actor for delegated decisions (Assist helper, not caster)", () => {
    // CR 702.132a: the chosen helper acts, not the caster. waitingPlayer routes
    // this; useTurnStatus must surface the helper as the waiting seat.
    setup({
      seat: 0,
      waitingFor: { type: "AssistPayment", data: { caster: 1, chosen: 0, max_generic: 3 } },
      state: { active_player: 1 },
    });
    const { result } = renderHook(() => useTurnStatus());
    expect(result.current.waitingSeatId).toBe(0);
    expect(result.current.canIActNow).toBe(true);
  });
});
