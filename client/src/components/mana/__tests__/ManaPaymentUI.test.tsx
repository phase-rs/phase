import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { ManaPaymentUI } from "../ManaPaymentUI";
import { useGameStore } from "../../../stores/gameStore";
import type { GameState } from "../../../adapter/types";

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
    objects: {},
    next_object_id: 1,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: {
      type: "ManaPayment",
      data: { player: 0 },
    },
    has_pending_cast: false,
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
      uses_commander: false,

      allow_debug_actions: false,
    },
    eliminated_players: [],
    ...overrides,
  };
}

describe("ManaPaymentUI", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  afterEach(() => {
    cleanup();
  });

  it("renders cancel during mana payment when no top-stack spell cost can be inferred", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState();

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [{ type: "CancelCast" }, { type: "PassPriority" }],
      });
    });

    render(<ManaPaymentUI />);

    expect(screen.getByText("Payment is still pending. Tap permanents or cancel this action.")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(dispatch).toHaveBeenCalledWith({ type: "CancelCast" });
  });

  it("shows the convoke payment hint during convoke mana payment", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const spellObj = {
      id: 300,
      name: "Venerated Loxodon",
      controller: 0,
      owner: 0,
      card_id: 3,
      mana_cost: {
        type: "Cost",
        shards: ["White"],
        generic: 4,
      },
      zone: "Stack",
      tapped: false,
      card_types: { core_types: ["Creature"], subtypes: [], supertypes: [] },
      abilities: [],
      colors: ["White"],
      counters: {},
      damage: 0,
      is_summon_sick: false,
      attached_to: null,
      cast_from_zone: null,
      face_down: false,
      is_commander: false,
      is_attacking: null,
      is_blocking: null,
      mana_spent_to_cast: false,
      colors_spent_to_cast: { W: 0, U: 0, B: 0, R: 0, G: 0, C: 0 },
    } as unknown as GameState["objects"][number];
    const gameState = createGameState({
      objects: { 300: spellObj },
      stack: [
        {
          id: 300,
          source_id: 300,
          controller: 0,
          kind: {
            type: "Spell",
            card_id: 3,
            ability: null,
            casting_variant: { type: "Normal" },
            actual_mana_spent: 0,
          },
        },
      ] as unknown as GameState["stack"],
      waiting_for: {
        type: "ManaPayment",
        data: { player: 0, convoke_mode: "Convoke" },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [{ type: "CancelCast" }, { type: "PassPriority" }],
      });
    });

    render(<ManaPaymentUI />);

    expect(screen.getByText("Tap creatures to help pay.")).toBeInTheDocument();
  });

  // CR 107.4f + CR 601.2f: When the engine reports PhyrexianPayment, clicking Pay
  // dispatches SubmitPhyrexianChoices with one choice per shard (default: PayMana).
  it("dispatches SubmitPhyrexianChoices with defaults for PhyrexianPayment", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const spellObj = {
      id: 100,
      name: "Gitaxian Probe",
      controller: 0,
      owner: 0,
      card_id: 1,
      mana_cost: {
        type: "Cost",
        shards: ["PhyrexianBlue"],
        generic: 0,
      },
      zone: "Stack",
      tapped: false,
      card_types: { core_types: ["Instant"], subtypes: [], supertypes: [] },
      abilities: [],
      colors: [],
      counters: {},
      damage: 0,
      is_summon_sick: false,
      attached_to: null,
      cast_from_zone: null,
      face_down: false,
      is_commander: false,
      is_attacking: null,
      is_blocking: null,
      mana_spent_to_cast: false,
      colors_spent_to_cast: { W: 0, U: 0, B: 0, R: 0, G: 0, C: 0 },
    } as unknown as GameState["objects"][number];

    const gameState = createGameState({
      objects: { 100: spellObj },
      stack: [
        {
          id: 100,
          source_id: 100,
          controller: 0,
          kind: {
            type: "Spell",
            card_id: 1,
            ability: null,
            casting_variant: { type: "Normal" },
            actual_mana_spent: 0,
          },
        },
      ] as unknown as GameState["stack"],
      waiting_for: {
        type: "PhyrexianPayment",
        data: {
          player: 0,
          spell_object: 100,
          shards: [
            {
              shard_index: 0,
              color: "Blue",
              options: { type: "ManaOrLife" },
            },
          ],
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [
          { type: "CancelCast" },
          {
            type: "SubmitPhyrexianChoices",
            data: { choices: [{ type: "PayMana" }] },
          },
        ],
      });
    });

    render(<ManaPaymentUI />);
    fireEvent.click(screen.getByRole("button", { name: "Pay" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "SubmitPhyrexianChoices",
      data: { choices: [{ type: "PayMana" }] },
    });
  });

  // Issue #457 — CR 601.2f: ManaPaymentUI must display the engine-resolved
  // locked-in cost (`pending_cast.cost`), not the printed base `mana_cost`.
  // Call the Coppercoats is a Strive spell; with multiple target opponents the
  // engine inflates {2}{W} to {4}{W}{W}{W}. The panel must show the inflated total.
  it("displays the Strive-inflated pending_cast cost, not the printed mana_cost", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const spellObj = {
      id: 400,
      name: "Call the Coppercoats",
      controller: 0,
      owner: 0,
      card_id: 4,
      // Printed base cost {2}{W} — must NOT be what the panel shows.
      mana_cost: { type: "Cost", shards: ["White"], generic: 2 },
      zone: "Stack",
      tapped: false,
      card_types: { core_types: ["Instant"], subtypes: [], supertypes: [] },
      abilities: [],
      colors: ["White"],
      counters: {},
      damage: 0,
      is_summon_sick: false,
      attached_to: null,
      cast_from_zone: null,
      face_down: false,
      is_commander: false,
      is_attacking: null,
      is_blocking: null,
      mana_spent_to_cast: false,
      colors_spent_to_cast: { W: 0, U: 0, B: 0, R: 0, G: 0, C: 0 },
    } as unknown as GameState["objects"][number];
    const gameState = createGameState({
      objects: { 400: spellObj },
      stack: [
        {
          id: 400,
          source_id: 400,
          controller: 0,
          kind: {
            type: "Spell",
            card_id: 4,
            ability: null,
            casting_variant: { type: "Normal" },
            actual_mana_spent: 0,
          },
        },
      ] as unknown as GameState["stack"],
      // Engine-resolved locked-in total: {2}{W} + 2 × {1}{W} Strive surcharge.
      pending_cast: {
        object_id: 400,
        cost: { type: "Cost", shards: ["White", "White", "White"], generic: 4 },
      } as unknown as GameState["pending_cast"],
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [{ type: "CancelCast" }, { type: "PassPriority" }],
      });
    });

    render(<ManaPaymentUI />);

    // ManaSymbol renders each shard as an <img> with alt={shard}. The inflated
    // total {4}{W}{W}{W} → generic shard "4" plus three "W" shards.
    expect(screen.getByAltText("4")).toBeInTheDocument();
    expect(screen.getAllByAltText("W")).toHaveLength(3);
    // The base printed cost generic of 2 must NOT appear.
    expect(screen.queryByAltText("2")).not.toBeInTheDocument();
  });

  // Regression guard — no Strive, no statics: pending_cast.cost equals the
  // printed mana_cost, so the panel renders the unchanged base cost.
  it("renders the base cost when pending_cast.cost equals the printed mana_cost", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const spellObj = {
      id: 500,
      name: "Plain Spell",
      controller: 0,
      owner: 0,
      card_id: 5,
      mana_cost: { type: "Cost", shards: ["White"], generic: 2 },
      zone: "Stack",
      tapped: false,
      card_types: { core_types: ["Instant"], subtypes: [], supertypes: [] },
      abilities: [],
      colors: ["White"],
      counters: {},
      damage: 0,
      is_summon_sick: false,
      attached_to: null,
      cast_from_zone: null,
      face_down: false,
      is_commander: false,
      is_attacking: null,
      is_blocking: null,
      mana_spent_to_cast: false,
      colors_spent_to_cast: { W: 0, U: 0, B: 0, R: 0, G: 0, C: 0 },
    } as unknown as GameState["objects"][number];
    const gameState = createGameState({
      objects: { 500: spellObj },
      stack: [
        {
          id: 500,
          source_id: 500,
          controller: 0,
          kind: {
            type: "Spell",
            card_id: 5,
            ability: null,
            casting_variant: { type: "Normal" },
            actual_mana_spent: 0,
          },
        },
      ] as unknown as GameState["stack"],
      pending_cast: {
        object_id: 500,
        cost: { type: "Cost", shards: ["White"], generic: 2 },
      } as unknown as GameState["pending_cast"],
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [{ type: "CancelCast" }, { type: "PassPriority" }],
      });
    });

    render(<ManaPaymentUI />);

    expect(screen.getByAltText("2")).toBeInTheDocument();
    expect(screen.getByAltText("W")).toBeInTheDocument();
  });

  it("displays activated-ability mana cost from pending_cast.activation_cost when present", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const sourceObj = {
      id: 700,
      name: "The Reality Chip",
      controller: 0,
      owner: 0,
      card_id: 7,
      mana_cost: { type: "Cost", shards: ["Blue"], generic: 2 },
      zone: "Battlefield",
      tapped: false,
      card_types: { core_types: ["Artifact"], subtypes: ["Equipment"], supertypes: [] },
      abilities: [],
      colors: [],
      counters: {},
      damage: 0,
      is_summon_sick: false,
      attached_to: null,
      cast_from_zone: null,
      face_down: false,
      is_commander: false,
      is_attacking: null,
      is_blocking: null,
      mana_spent_to_cast: false,
      colors_spent_to_cast: { W: 0, U: 0, B: 0, R: 0, G: 0, C: 0 },
    } as unknown as GameState["objects"][number];

    const gameState = createGameState({
      objects: { 700: sourceObj },
      pending_cast: {
        object_id: 700,
        // Spells use `cost`; activated abilities use `activation_cost`.
        cost: { type: "NoCost" },
        activation_cost: {
          type: "Mana",
          cost: { type: "Cost", shards: ["Blue"], generic: 2 },
        },
        activation_ability_index: 0,
      } as unknown as GameState["pending_cast"],
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [{ type: "CancelCast" }, { type: "PassPriority" }],
      });
    });

    render(<ManaPaymentUI />);

    // Should display activation cost {2}{U}.
    expect(screen.getByAltText("2")).toBeInTheDocument();
    expect(screen.getByAltText("U")).toBeInTheDocument();
  });

  // pending_cast absent — fall back to the stack spell object's mana_cost.
  it("falls back to the stack spell object mana_cost when pending_cast is absent", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const spellObj = {
      id: 600,
      name: "Fallback Spell",
      controller: 0,
      owner: 0,
      card_id: 6,
      mana_cost: { type: "Cost", shards: ["Blue"], generic: 3 },
      zone: "Stack",
      tapped: false,
      card_types: { core_types: ["Instant"], subtypes: [], supertypes: [] },
      abilities: [],
      colors: ["Blue"],
      counters: {},
      damage: 0,
      is_summon_sick: false,
      attached_to: null,
      cast_from_zone: null,
      face_down: false,
      is_commander: false,
      is_attacking: null,
      is_blocking: null,
      mana_spent_to_cast: false,
      colors_spent_to_cast: { W: 0, U: 0, B: 0, R: 0, G: 0, C: 0 },
    } as unknown as GameState["objects"][number];
    const gameState = createGameState({
      objects: { 600: spellObj },
      stack: [
        {
          id: 600,
          source_id: 600,
          controller: 0,
          kind: {
            type: "Spell",
            card_id: 6,
            ability: null,
            casting_variant: { type: "Normal" },
            actual_mana_spent: 0,
          },
        },
      ] as unknown as GameState["stack"],
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [{ type: "CancelCast" }, { type: "PassPriority" }],
      });
    });

    render(<ManaPaymentUI />);

    expect(screen.getByAltText("3")).toBeInTheDocument();
    expect(screen.getByAltText("U")).toBeInTheDocument();
  });

  // CR 107.4f: With PayLife toggled on a ManaOrLife shard, dispatch carries PayLife.
  it("dispatches PayLife when the shard toggle is flipped", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const spellObj = {
      id: 200,
      name: "Dismember",
      controller: 0,
      owner: 0,
      card_id: 2,
      mana_cost: {
        type: "Cost",
        shards: ["PhyrexianBlack", "PhyrexianBlack", "PhyrexianBlack"],
        generic: 1,
      },
      zone: "Stack",
      tapped: false,
      card_types: { core_types: ["Instant"], subtypes: [], supertypes: [] },
      abilities: [],
      colors: [],
      counters: {},
      damage: 0,
      is_summon_sick: false,
      attached_to: null,
      cast_from_zone: null,
      face_down: false,
      is_commander: false,
      is_attacking: null,
      is_blocking: null,
      mana_spent_to_cast: false,
      colors_spent_to_cast: { W: 0, U: 0, B: 0, R: 0, G: 0, C: 0 },
    } as unknown as GameState["objects"][number];

    const gameState = createGameState({
      objects: { 200: spellObj },
      stack: [
        {
          id: 200,
          source_id: 200,
          controller: 0,
          kind: {
            type: "Spell",
            card_id: 2,
            ability: null,
            casting_variant: { type: "Normal" },
            actual_mana_spent: 0,
          },
        },
      ] as unknown as GameState["stack"],
      waiting_for: {
        type: "PhyrexianPayment",
        data: {
          player: 0,
          spell_object: 200,
          shards: [
            {
              shard_index: 0,
              color: "Black",
              options: { type: "ManaOrLife" },
            },
            {
              shard_index: 1,
              color: "Black",
              options: { type: "ManaOrLife" },
            },
            {
              shard_index: 2,
              color: "Black",
              options: { type: "ManaOrLife" },
            },
          ],
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [],
      });
    });

    render(<ManaPaymentUI />);

    // Three Phyrexian toggle buttons plus Pay and Cancel. Pick the first toggle
    // by matching the gray-800 background (unselected mana state).
    const allButtons = screen.getAllByRole("button");
    const toggles = allButtons.filter((b) =>
      b.className.includes("bg-gray-800"),
    );
    expect(toggles.length).toBe(3);
    // Click the first Phyrexian toggle (defaults to mana); flips to PayLife.
    fireEvent.click(toggles[0]);

    fireEvent.click(screen.getByRole("button", { name: "Pay" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "SubmitPhyrexianChoices",
      data: {
        choices: [
          { type: "PayLife" },
          { type: "PayMana" },
          { type: "PayMana" },
        ],
      },
    });
  });
});
