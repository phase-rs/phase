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
