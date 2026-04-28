import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { PlayerArea } from "../PlayerArea.tsx";

vi.mock("../../../hooks/useCardImage", () => ({
  useCardImage: () => ({ src: null, isLoading: false }),
}));

function commanderObject(overrides: Partial<GameObject> = {}): GameObject {
  return {
    id: 101,
    card_id: 201,
    owner: 1,
    controller: 1,
    zone: "Command",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name: "Opponent Commander",
    power: 3,
    toughness: 3,
    loyalty: null,
    card_types: { supertypes: ["Legendary"], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "Cost", shards: ["Green"], generic: 2 },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: ["Green"],
    base_power: 3,
    base_toughness: 3,
    base_keywords: [],
    base_color: ["Green"],
    timestamp: 1,
    entered_battlefield_turn: null,
    is_commander: true,
    commander_tax: 0,
    ...overrides,
  };
}

function createGameState(overrides: Partial<GameState> = {}): GameState {
  const commander = commanderObject();

  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 0,
    objects: { [commander.id]: commander },
    next_object_id: 102,
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
    next_timestamp: 2,
    format_config: {
      format: "Commander",
      starting_life: 40,
      min_players: 2,
      max_players: 4,
      deck_size: 100,
      singleton: true,
      command_zone: true,
      commander_damage_threshold: 21,
      range_of_influence: null,
      team_based: false,
    },
    command_zone: [commander.id],
    commander_damage: [],
    ...overrides,
  };
}

describe("PlayerArea", () => {
  beforeEach(() => {
    useGameStore.setState({
      gameState: createGameState(),
      legalActions: [],
      spellCosts: {},
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders an opponent commander as a command-zone card", () => {
    const { container } = render(<PlayerArea playerId={1} mode="focused" />);

    expect(
      container.querySelector('button[title="Commander: Opponent Commander"]'),
    ).toBeInTheDocument();
  });
});
