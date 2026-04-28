import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { ChooseXValueUI } from "../ChooseXValueUI";
import { useGameStore } from "../../../stores/gameStore";
import type { GameState, PendingCast, WaitingFor } from "../../../adapter/types";

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
    objects: {
      42: {
        id: 42,
        card_id: 1,
        name: "Nature's Rhythm",
        controller: 0,
        owner: 0,
        zone: "Stack",
      } as unknown as GameState["objects"][number],
    },
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

function createPendingCast(): PendingCast {
  return {
    object_id: 42,
    card_id: 1,
    ability: {} as PendingCast["ability"],
    cost: { type: "Cost", shards: ["X", "G", "G"], generic: 0 },
  };
}

function chooseXWaitingFor(max: number): WaitingFor {
  return {
    type: "ChooseXValue",
    data: { player: 0, max, pending_cast: createPendingCast() },
  };
}

describe("ChooseXValueUI", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  afterEach(() => {
    cleanup();
  });

  it("renders nothing when not in ChooseXValue state", () => {
    act(() => {
      useGameStore.setState({
        gameState: createGameState(),
        waitingFor: { type: "Priority", data: { player: 0 } },
        dispatch: vi.fn().mockResolvedValue([]),
      });
    });

    const { container } = render(<ChooseXValueUI />);
    expect(container).toBeEmptyDOMElement();
  });

  it("shows card name and dispatches ChooseX with selected value on confirm", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const waitingFor = chooseXWaitingFor(5);

    act(() => {
      useGameStore.setState({
        gameState: createGameState({ waiting_for: waitingFor }),
        waitingFor,
        dispatch,
      });
    });

    render(<ChooseXValueUI />);

    expect(screen.getByText(/Choose a value for X/)).toBeInTheDocument();
    expect(screen.getByText(/Nature's Rhythm/)).toBeInTheDocument();

    const slider = screen.getByLabelText("Choose X value") as HTMLInputElement;
    expect(slider.max).toBe("5");

    fireEvent.change(slider, { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: "Confirm X = 3" }));

    expect(dispatch).toHaveBeenCalledWith({ type: "ChooseX", data: { value: 3 } });
  });

  it("dispatches CancelCast when cancel is clicked", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const waitingFor = chooseXWaitingFor(3);

    act(() => {
      useGameStore.setState({
        gameState: createGameState({ waiting_for: waitingFor }),
        waitingFor,
        dispatch,
      });
    });

    render(<ChooseXValueUI />);

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(dispatch).toHaveBeenCalledWith({ type: "CancelCast" });
  });

  it("resets value to 0 when ChooseXValue state re-enters", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const waitingFor = chooseXWaitingFor(10);

    act(() => {
      useGameStore.setState({
        gameState: createGameState({ waiting_for: waitingFor }),
        waitingFor,
        dispatch,
      });
    });

    const { rerender } = render(<ChooseXValueUI />);

    const slider = screen.getByLabelText("Choose X value") as HTMLInputElement;
    fireEvent.change(slider, { target: { value: "7" } });
    expect(screen.getByRole("button", { name: "Confirm X = 7" })).toBeInTheDocument();

    // Simulate re-entering ChooseXValue (e.g., after cost reduction changes max)
    const nextWaitingFor = chooseXWaitingFor(4);
    act(() => {
      useGameStore.setState({
        gameState: createGameState({ waiting_for: nextWaitingFor }),
        waitingFor: nextWaitingFor,
        dispatch,
      });
    });

    rerender(<ChooseXValueUI />);

    expect(screen.getByRole("button", { name: "Confirm X = 0" })).toBeInTheDocument();
  });
});
