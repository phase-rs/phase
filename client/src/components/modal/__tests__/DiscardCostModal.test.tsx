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

function makeObject(id: number, name: string, zone: GameObject["zone"] = "Hand"): GameObject {
  return {
    id,
    card_id: id,
    owner: 0,
    controller: 0,
    zone,
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
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
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

describe("Discard cost modal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("allows cancelling discard costs", () => {
    setWaitingFor({
      type: "PayCost",
      data: {
        player: 0,
        kind: { type: "Discard" },
        choices: [],
        count: 1,
        min_count: 0,
        resume: { type: "Spell", Spell: {} },
      },
    } as unknown as WaitingFor);

    render(<CardChoiceModal />);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(dispatchMock).toHaveBeenCalledWith({ type: "CancelCast" });
  });

  it.each([
    [
      "PayCost ExileFromZone",
      {
        player: 0,
        kind: { type: "ExileFromZone", zone: "Graveyard" },
        choices: [],
        count: 1,
        min_count: 0,
        resume: { type: "Spell", Spell: {} },
      },
    ],
    [
      "CollectEvidenceChoice",
      {
        player: 0,
        minimum_mana_value: 1,
        cards: [],
        resume: {},
      },
    ],
  ])("allows cancelling %s", (label, data) => {
    // CollectEvidence keeps its own variant `type`; the PayCost-prefixed labels
    // all map to the unified `PayCost` variant.
    const type = label.startsWith("PayCost") ? "PayCost" : label;
    setWaitingFor({ type, data } as unknown as WaitingFor);

    render(<CardChoiceModal />);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(dispatchMock).toHaveBeenCalledWith({ type: "CancelCast" });
  });

  it.each([
    [
      "PayCost Sacrifice",
      {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "Sacrifice" },
          choices: [10],
          count: 1,
          min_count: 0,
          resume: { type: "Spell", Spell: {} },
        },
      },
      { 10: makeObject(10, "Food Token", "Battlefield") },
    ],
    [
      "PayCost ReturnToHand",
      {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "ReturnToHand" },
          choices: [10],
          count: 1,
          min_count: 0,
          resume: { type: "Spell", Spell: {} },
        },
      },
      { 10: makeObject(10, "Kor Skyfisher", "Battlefield") },
    ],
    [
      "BlightChoice",
      {
        type: "BlightChoice",
        data: {
          player: 0,
          count: 1,
          creatures: [],
          pending_cast: {},
        },
      },
      {},
    ],
    [
      "HarmonizeTapChoice",
      {
        type: "HarmonizeTapChoice",
        data: {
          player: 0,
          eligible_creatures: [],
          pending_cast: {},
        },
      },
      {},
    ],
  ])("suppresses the modal for board-native %s", (_label, waitingFor, objects) => {
    setWaitingFor(waitingFor as unknown as WaitingFor, objects);

    render(<CardChoiceModal />);

    expect(screen.queryByRole("button")).toBeNull();
  });

  it("handles discard prompts for mana ability costs", () => {
    setWaitingFor({
      type: "PayCost",
      data: {
        player: 0,
        kind: { type: "Discard" },
        choices: [],
        count: 1,
        min_count: 0,
        resume: { type: "ManaAbility", ManaAbility: {} },
      },
    } as unknown as WaitingFor);

    render(<CardChoiceModal />);

    expect(screen.getByText("Discard for mana ability")).toBeInTheDocument();
  });

  it("describes untap selection without saying sacrifice", () => {
    setWaitingFor(
      {
        type: "EffectZoneChoice",
        data: {
          player: 0,
          cards: [10, 11],
          count: 5,
          min_count: 0,
          up_to: true,
          source_id: 1,
          effect_kind: "Untap",
          zone: "Battlefield",
        },
      } as unknown as WaitingFor,
      {
        10: { ...makeObject(10, "Island"), zone: "Battlefield" },
        11: { ...makeObject(11, "Forest"), zone: "Battlefield" },
      },
    );

    render(<CardChoiceModal />);

    expect(screen.getByText("Untap")).toBeInTheDocument();
    expect(screen.getByText("Choose up to 5 permanents to untap")).toBeInTheDocument();
    expect(screen.queryByText(/sacrifice/i)).not.toBeInTheDocument();
  });

  it("describes optional attach selection without saying sacrifice and allows decline", () => {
    setWaitingFor(
      {
        type: "EffectZoneChoice",
        data: {
          player: 0,
          cards: [10, 11],
          count: 2,
          min_count: 0,
          up_to: true,
          source_id: 19,
          effect_kind: "Attach",
          zone: "Battlefield",
        },
      } as unknown as WaitingFor,
      {
        10: { ...makeObject(10, "S.H.I.E.L.D. Spy Kit"), zone: "Battlefield" },
        11: { ...makeObject(11, "Vibranium Energy Daggers"), zone: "Battlefield" },
      },
    );

    render(<CardChoiceModal />);

    expect(screen.getByText("Attach")).toBeInTheDocument();
    expect(screen.getByText("Choose up to 2 Equipment to attach")).toBeInTheDocument();
    expect(screen.queryByText(/sacrifice/i)).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Decline" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [] },
    });
  });

  it("describes library placement without saying battlefield", () => {
    setWaitingFor({
      type: "EffectZoneChoice",
      data: {
        player: 0,
        cards: [],
        count: 2,
        min_count: 0,
        up_to: false,
        source_id: 1,
        effect_kind: "PutAtLibraryPosition",
        zone: "Hand",
      },
    } as unknown as WaitingFor);

    render(<CardChoiceModal />);

    expect(screen.getByText("Put on Library")).toBeInTheDocument();
    expect(screen.getByText("Choose 2 cards to put on top of your library")).toBeInTheDocument();
    expect(screen.queryByText(/battlefield/i)).not.toBeInTheDocument();
  });

  it("suppresses battlefield return choices for board-native selection", () => {
    setWaitingFor(
      {
        type: "EffectZoneChoice",
        data: {
          player: 0,
          cards: [10],
          count: 1,
          min_count: 0,
          up_to: false,
          source_id: 1,
          effect_kind: "ReturnToHand",
          zone: "Battlefield",
          destination: "Hand",
        },
      } as unknown as WaitingFor,
      {
        10: { ...makeObject(10, "Kor Skyfisher"), zone: "Battlefield" },
      },
    );

    render(<CardChoiceModal />);

    expect(screen.queryByRole("button")).toBeNull();
    expect(dispatchMock).not.toHaveBeenCalled();
  });

  it("shows topdeck order and dispatches selected cards in click order", () => {
    setWaitingFor(
      {
        type: "EffectZoneChoice",
        data: {
          player: 0,
          cards: [10, 11],
          count: 2,
          min_count: 0,
          up_to: false,
          source_id: 1,
          effect_kind: "PutAtLibraryPosition",
          zone: "Hand",
        },
      } as unknown as WaitingFor,
      {
        10: makeObject(10, "First Card"),
        11: makeObject(11, "Second Card"),
      },
    );

    render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: /Second Card/i }));
    fireEvent.click(screen.getByRole("button", { name: /First Card/i }));

    expect(screen.getByText("2nd")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Put on top (Top -> 2nd)" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [11, 10] },
    });
  });
});
