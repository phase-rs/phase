import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeObject(id: number, name: string): GameObject {
  return {
    id,
    card_id: id,
    owner: 0,
    controller: 0,
    zone: "Library",
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
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Instant"], subtypes: [] },
    mana_cost: { type: "Cost", shards: [], generic: 1 },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: [],
    base_power: null,
    base_toughness: null,
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
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [42, 43], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 0,
    objects,
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

function setWaitingFor(waitingFor: WaitingFor, objects: Record<string, GameObject>) {
  const state = makeState(waitingFor, objects);
  useGameStore.setState({
    gameMode: "online",
    gameState: state,
    waitingFor,
  });
}

describe("SearchPartition modal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  // Final Parting shape: primary goes to hand, the rest to the graveyard. The
  // subtitle must name the REAL destinations, not the hard-coded battlefield.
  it("names the real primary and rest destination zones", async () => {
    setWaitingFor(
      {
        type: "SearchPartitionChoice",
        data: {
          player: 0,
          cards: [42, 43],
          primary_destination: "Hand",
          primary_count: 1,
          primary_enter_tapped: false,
          rest_destination: "Graveyard",
          source_id: 99,
        },
      } as WaitingFor,
      { 42: makeObject(42, "Lightning Bolt"), 43: makeObject(43, "Dark Ritual") },
    );

    render(<CardChoiceModal />);

    expect(await screen.findByText(/your hand/i)).toBeInTheDocument();
    expect(await screen.findByText(/your graveyard/i)).toBeInTheDocument();
    // Revert-failing: the pre-fix modal hard-coded "the battlefield" and never
    // mentioned the graveyard.
    expect(screen.queryByText(/battlefield/i)).not.toBeInTheDocument();
  });
});
