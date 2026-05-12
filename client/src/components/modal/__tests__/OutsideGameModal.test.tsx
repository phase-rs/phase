import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeState(waitingFor: WaitingFor): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [
      {
        id: 0,
        life: 20,
        poison_counters: 0,
        mana_pool: { mana: [] },
        library: [],
        hand: [],
        graveyard: [],
        has_drawn_this_turn: false,
        lands_played_this_turn: 0,
        turns_taken: 0,
      },
      {
        id: 1,
        life: 20,
        poison_counters: 0,
        mana_pool: { mana: [] },
        library: [],
        hand: [],
        graveyard: [],
        has_drawn_this_turn: false,
        lands_played_this_turn: 0,
        turns_taken: 0,
      },
    ],
    priority_player: 0,
    objects: {},
    next_object_id: 100,
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
    next_timestamp: 2,
    eliminated_players: [],
  } as unknown as GameState;
}

function outsideGameChoice(count: number): WaitingFor {
  return {
    type: "OutsideGameChoice",
    data: {
      player: 0,
      choices: [
        {
          sideboard_index: 0,
          entry: {
            card: { name: "Pyroclasm" },
            count,
          },
        },
      ],
      count: 1,
      reveal: true,
      up_to: false,
      destination: "Hand",
    },
  };
}

function setWaitingFor(waitingFor: WaitingFor) {
  useGameStore.setState({
    gameMode: "online",
    gameState: makeState(waitingFor),
    waitingFor,
  });
}

describe("OutsideGameModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("clears selected copies synchronously when a new outside-game prompt replaces the old one", () => {
    setWaitingFor(outsideGameChoice(2));
    const { rerender } = render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: /Pyroclasm/ }));
    expect(screen.getByRole("button", { name: "Confirm" })).toBeEnabled();

    setWaitingFor(outsideGameChoice(1));
    rerender(<CardChoiceModal />);

    expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();
    expect(dispatchMock).not.toHaveBeenCalled();
  });
});
