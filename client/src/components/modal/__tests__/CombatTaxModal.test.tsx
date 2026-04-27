import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { CombatTaxModal } from "../CombatTaxModal.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeObject(id: number, name: string) {
  return {
    id,
    card_id: 1,
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
    power: 2,
    toughness: 2,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "NoCost" as const },
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
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0 },
    ],
    priority_player: 0,
    objects: {
      101: makeObject(101, "Grizzly Bears"),
      102: makeObject(102, "Llanowar Elves"),
    },
    next_object_id: 200,
    battlefield: [101, 102],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: {
      type: "CombatTaxPayment",
      data: {
        player: 0,
        context: { type: "Attacking" },
        total_cost: { type: "Cost", shards: [], generic: 4 },
        per_creature: [
          [101, { type: "Cost", shards: [], generic: 2 }],
          [102, { type: "Cost", shards: [], generic: 2 }],
        ],
        pending: {
          type: "Attack",
          data: {
            attacks: [
              [101, { type: "Player", data: 1 }],
              [102, { type: "Player", data: 1 }],
            ],
          },
        },
      },
    },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 2,
    eliminated_players: [],
  };
}

describe("CombatTaxModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
    const state = makeState();
    useGameStore.setState({
      gameState: state,
      waitingFor: state.waiting_for,
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders per-creature breakdown and dispatches accept=true on Pay", () => {
    render(<CombatTaxModal />);

    expect(screen.getByText(/Pay to Attack/)).toBeInTheDocument();
    expect(screen.getByText("Grizzly Bears")).toBeInTheDocument();
    expect(screen.getByText("Llanowar Elves")).toBeInTheDocument();

    // Pay action triggers accept=true
    fireEvent.click(screen.getByRole("button", { name: /^Pay/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "PayCombatTax",
      data: { accept: true },
    });
  });

  it("dispatches accept=false on decline", () => {
    render(<CombatTaxModal />);

    fireEvent.click(screen.getByRole("button", { name: /Decline/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "PayCombatTax",
      data: { accept: false },
    });
  });

  it("renders Pay to Block title when context is Blocking", () => {
    const state = makeState();
    state.waiting_for = {
      type: "CombatTaxPayment",
      data: {
        ...(state.waiting_for as Extract<typeof state.waiting_for, { type: "CombatTaxPayment" }>).data,
        context: { type: "Blocking" },
        pending: {
          type: "Block",
          data: { assignments: [[101, 999]] },
        },
      },
    };
    useGameStore.setState({ gameState: state, waitingFor: state.waiting_for });

    render(<CombatTaxModal />);
    expect(screen.getByText(/Pay to Block/)).toBeInTheDocument();
  });
});
