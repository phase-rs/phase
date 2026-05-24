import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

import { StackEntry } from "../StackEntry.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import type { GameState, StackEntry as StackEntryType } from "../../../adapter/types.ts";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: "/test-card.png", isLoading: false }),
}));

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
    next_object_id: 100,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
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

describe("StackEntry", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  afterEach(() => {
    cleanup();
  });

  it("renders the live pending_cast cost for an in-flight X spell instead of the printed base cost", () => {
    const entry = {
      id: 77,
      source_id: 42,
      controller: 0,
      kind: {
        type: "Spell",
        card_id: 1,
        ability: null,
        casting_variant: { type: "Normal" },
        actual_mana_spent: 0,
      },
    } as unknown as StackEntryType;

    const gameState = createGameState({
      objects: {
        42: {
          id: 42,
          card_id: 1,
          name: "Crackle with Power",
          controller: 0,
          owner: 0,
          zone: "Stack",
          mana_cost: { type: "Cost", shards: ["X", "Red", "Red"], generic: 2 },
          tapped: false,
          card_types: { core_types: ["Sorcery"], subtypes: [], supertypes: [] },
          abilities: [],
          colors: ["Red"],
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
        } as unknown as GameState["objects"][number],
      },
      stack: [entry] as unknown as GameState["stack"],
      waiting_for: {
        type: "ChooseXValue",
        data: {
          player: 0,
          min: 0,
          max: 3,
          pending_cast: {
            object_id: 42,
            card_id: 1,
            ability: {} as never,
            cost: { type: "Cost", shards: ["X", "Red", "Red"], generic: 0 },
          },
        },
      } as GameState["waiting_for"],
      has_pending_cast: true,
      pending_cast: {
        object_id: 42,
        card_id: 1,
        ability: {} as never,
        cost: { type: "Cost", shards: ["X", "Red", "Red"], generic: 0 },
      } as GameState["pending_cast"],
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
      });
    });

    render(
      <StackEntry
        entry={entry}
        index={0}
        isTop
        isPending
        cardSize={{ width: 120, height: 168 }}
      />,
    );

    expect(screen.getByAltText("X")).toBeInTheDocument();
    expect(screen.getAllByAltText("R")).toHaveLength(2);
    expect(screen.queryByAltText("2")).not.toBeInTheDocument();
  });
});
