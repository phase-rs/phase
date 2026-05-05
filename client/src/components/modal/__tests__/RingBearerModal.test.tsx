import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeCreature(id: number, name: string) {
  return {
    id,
    card_id: id,
    owner: 0,
    controller: 0,
    zone: "Battlefield" as const,
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name,
    power: 1,
    toughness: 1,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "NoCost" as const },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: [],
    base_power: 1,
    base_toughness: 1,
    base_keywords: [],
    base_color: [],
    timestamp: 1,
    entered_battlefield_turn: 1,
  };
}

function makeState(): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "DeclareAttackers",
    players: [
      { id: 0, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 0,
    objects: {
      42: makeCreature(42, "Frodo Baggins"),
      43: makeCreature(43, "Samwise Gamgee"),
    },
    next_object_id: 100,
    battlefield: [42, 43],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: {
      type: "ChooseRingBearer",
      data: { player: 0, candidates: [42, 43] },
    },
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

describe("RingBearerModal (via CardChoiceModal)", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    const state = makeState();
    useMultiplayerStore.setState({ activePlayerId: 0 });
    useGameStore.setState({
      gameMode: "online",
      gameState: state,
      waitingFor: state.waiting_for,
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("dispatches the selected ring-bearer candidate", () => {
    render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: "Samwise Gamgee" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseRingBearer",
      data: { target: 43 },
    });
  });
});
