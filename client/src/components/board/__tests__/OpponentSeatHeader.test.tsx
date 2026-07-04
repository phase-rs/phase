import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, TargetRef, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { OpponentSeatHeader } from "../OpponentSeatHeader.tsx";

function targetSelectionWaitingFor(legalPlayers: number[]): WaitingFor {
  const targets: TargetRef[] = legalPlayers.map((player) => ({ Player: player }));
  return {
    type: "TargetSelection",
    data: {
      player: 0,
      selection: {
        current_slot: 0,
        current_legal_targets: targets,
      },
      target_slots: [{ legal_targets: targets }],
      pending_cast: {} as never,
    },
  } as WaitingFor;
}

function createGameState(waitingFor: WaitingFor): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [0, 1, 2, 3].map((id) => ({
      id,
      life: 40,
      poison_counters: 0,
      mana_pool: { mana: [] },
      library: [],
      hand: [],
      graveyard: [],
      has_drawn_this_turn: false,
      lands_played_this_turn: 0,
      turns_taken: 0,
    })),
    priority_player: 0,
    objects: {},
    next_object_id: 1,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: waitingFor,
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
    seat_order: [0, 1, 2, 3],
    eliminated_players: [],
  } as unknown as GameState;
}

describe("OpponentSeatHeader", () => {
  beforeEach(() => {
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("targets the opponent when the whole legal target plate is clicked", () => {
    const dispatch = vi.fn();
    const waitingFor = targetSelectionWaitingFor([1]);
    useGameStore.setState({
      dispatch,
      gameState: createGameState(waitingFor),
      waitingFor,
    });

    render(<OpponentSeatHeader playerId={1} />);

    fireEvent.click(screen.getByRole("button", { name: "Target Opp 2" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: { Player: 1 } },
    });
  });

  it("does not target when the opponent player is not legal", () => {
    const dispatch = vi.fn();
    const waitingFor = targetSelectionWaitingFor([2]);
    useGameStore.setState({
      dispatch,
      gameState: createGameState(waitingFor),
      waitingFor,
    });

    render(<OpponentSeatHeader playerId={1} />);

    fireEvent.click(screen.getByTestId("opponent-seat-header-1"));

    expect(screen.queryByRole("button", { name: "Target Opp 2" })).not.toBeInTheDocument();
    expect(dispatch).not.toHaveBeenCalled();
  });
});
