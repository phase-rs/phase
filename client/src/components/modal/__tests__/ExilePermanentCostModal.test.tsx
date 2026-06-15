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

function makeObject(id: number, name: string): GameObject {
  return {
    id,
    card_id: id,
    owner: 0,
    controller: 0,
    zone: "Battlefield",
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
    card_types: { supertypes: [], core_types: ["Land"], subtypes: [] },
    mana_cost: { type: "Cost", shards: [], generic: 0 },
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

function makeState(waitingFor: WaitingFor, objects: Record<string, GameObject> = {}): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
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
    has_pending_cast: true,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 2,
    eliminated_players: [],
  } as unknown as GameState;
}

function setWaitingFor(waitingFor: WaitingFor, objects?: Record<string, GameObject>) {
  const state = makeState(waitingFor, objects);
  useGameStore.setState({
    gameMode: "online",
    gameState: state,
    waitingFor,
  });
}

// CR 601.2h + CR 701.13: Lunar Hatchling's escape "Exile a land you control"
// surfaces a `PayCost { kind: ExilePermanent }` state. These tests guard the
// frontend wiring so the player is never softlocked with no modal (the bug:
// the `PayCostDispatch` switch had no `ExilePermanent` arm and returned
// `undefined`).
describe("Exile-permanent cost modal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders a modal for the ExilePermanent cost kind (no softlock)", () => {
    setWaitingFor(
      {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "ExilePermanent", filter: null },
          choices: [10],
          count: 1,
          min_count: 1,
          resume: { type: "Spell", Spell: {} },
        },
      } as unknown as WaitingFor,
      { 10: makeObject(10, "Forest") },
    );

    render(<CardChoiceModal />);

    // Title + subtitle resolve from the new `cardChoice.exilePermanent` keys.
    expect(screen.getByText("Exile a permanent")).toBeInTheDocument();
    expect(screen.getByText("Exile 1 permanent you control")).toBeInTheDocument();
    // The eligible permanent is offered for selection.
    expect(screen.getByRole("button", { name: /Forest/i })).toBeInTheDocument();
  });

  it("pluralizes the subtitle for multi-count fixed costs", () => {
    setWaitingFor(
      {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "ExilePermanent", filter: null },
          choices: [10, 11],
          count: 2,
          min_count: 2,
          resume: { type: "Spell", Spell: {} },
        },
      } as unknown as WaitingFor,
      { 10: makeObject(10, "Forest"), 11: makeObject(11, "Island") },
    );

    render(<CardChoiceModal />);

    expect(screen.getByText("Exile 2 permanents you control")).toBeInTheDocument();
  });

  it("uses the range subtitle when min_count differs from count", () => {
    setWaitingFor(
      {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "ExilePermanent", filter: null },
          choices: [10, 11, 12],
          count: 3,
          min_count: 1,
          resume: { type: "Spell", Spell: {} },
        },
      } as unknown as WaitingFor,
      {
        10: makeObject(10, "Forest"),
        11: makeObject(11, "Island"),
        12: makeObject(12, "Mountain"),
      },
    );

    render(<CardChoiceModal />);

    expect(screen.getByText("Exile 1 to 3 permanents you control")).toBeInTheDocument();
  });

  it("dispatches the selected permanent on confirm", () => {
    setWaitingFor(
      {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "ExilePermanent", filter: null },
          choices: [10],
          count: 1,
          min_count: 1,
          resume: { type: "Spell", Spell: {} },
        },
      } as unknown as WaitingFor,
      { 10: makeObject(10, "Forest") },
    );

    render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: /Forest/i }));
    fireEvent.click(screen.getByRole("button", { name: "Exile (1/1)" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [10] },
    });
  });

  it("allows cancelling the cost", () => {
    setWaitingFor(
      {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "ExilePermanent", filter: null },
          choices: [10],
          count: 1,
          min_count: 1,
          resume: { type: "Spell", Spell: {} },
        },
      } as unknown as WaitingFor,
      { 10: makeObject(10, "Forest") },
    );

    render(<CardChoiceModal />);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(dispatchMock).toHaveBeenCalledWith({ type: "CancelCast" });
  });
});
