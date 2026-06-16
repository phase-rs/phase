import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeCreature(id: number, name: string): GameObject {
  return {
    id,
    card_id: id,
    owner: 0,
    controller: 0,
    zone: "Battlefield",
    tapped: true,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name,
    power: 2,
    toughness: 2,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "Cost", shards: [], generic: 1 },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: [],
    base_power: 2,
    base_toughness: 2,
    base_keywords: [],
    base_color: [],
    timestamp: id,
    entered_battlefield_turn: null,
  };
}

function makeState(waitingFor: WaitingFor, objects: Record<string, GameObject>): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "Untap",
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 0,
    objects,
    next_object_id: 100,
    battlefield: Object.keys(objects).map((k) => Number(k)),
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

function setWaitingFor(waitingFor: WaitingFor, objects: Record<string, GameObject>) {
  const state = makeState(waitingFor, objects);
  useGameStore.setState({
    gameMode: "online",
    gameState: state,
    waitingFor,
  });
}

describe("ChooseUntapSubset modal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  // CR 502.3: a max-untap cap ("can't untap more than one <type>") bounds the
  // untap count from above only — choosing ZERO is legal (the whole group stays
  // tapped). The Confirm button must be enabled with an empty selection so a
  // human can decline to untap any of the capped permanents.
  it("confirms an empty selection (untap zero permanents)", () => {
    setWaitingFor(
      {
        type: "ChooseUntapSubset",
        data: { player: 0, group: [10, 11], max: 1 },
      } as WaitingFor,
      { 10: makeCreature(10, "Bear A"), 11: makeCreature(11, "Bear B") },
    );

    render(<CardChoiceModal />);

    // With nothing selected the confirm control is still actionable.
    const confirm = screen.getByRole("button", { name: /untap \(0\/1\)/i });
    expect(confirm).not.toBeDisabled();

    fireEvent.click(confirm);

    expect(dispatchMock).toHaveBeenCalledTimes(1);
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [] },
    });
  });

  it("confirms a bounded non-empty selection", () => {
    setWaitingFor(
      {
        type: "ChooseUntapSubset",
        data: { player: 0, group: [10, 11], max: 1 },
      } as WaitingFor,
      { 10: makeCreature(10, "Bear A"), 11: makeCreature(11, "Bear B") },
    );

    render(<CardChoiceModal />);

    // Select the first capped permanent (cap of 1 allows exactly one).
    fireEvent.click(screen.getByRole("button", { name: /Bear A/i }));

    const confirm = screen.getByRole("button", { name: /untap \(1\/1\)/i });
    expect(confirm).not.toBeDisabled();
    fireEvent.click(confirm);

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [10] },
    });
  });
});
