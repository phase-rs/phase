import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { ArchenemyPanel } from "../ArchenemyPanel.tsx";

function schemeObject(overrides: Partial<GameObject> = {}): GameObject {
  return {
    id: 77,
    card_id: 177,
    owner: 0,
    controller: 0,
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
    name: "Your Puny Minds Cannot Fathom",
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Scheme"], subtypes: [] },
    mana_cost: { type: "NoCost" },
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
    timestamp: 1,
    entered_battlefield_turn: null,
    is_commander: false,
    commander_tax: 0,
    ...overrides,
  };
}

function stateWithArchenemy(overrides: Partial<GameState> = {}): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [],
    priority_player: 0,
    objects: {
      77: schemeObject(),
    },
    next_object_id: 78,
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
    turn_has_attack_phase: true,
    consecutive_priority_passes: 0,
    pending_triggers: [],
    derived: {
      archenemy: {
        archenemy: 0,
        scheme_deck_count: 19,
        active_scheme_ids: [77],
        hero_player_ids: [1, 2],
      },
    },
    ...overrides,
  } as GameState;
}

describe("ArchenemyPanel", () => {
  beforeEach(() => {
    useGameStore.setState({ gameState: null });
  });

  afterEach(() => {
    cleanup();
    useGameStore.setState({ gameState: null });
  });

  it("does not render outside Archenemy", () => {
    const { container } = render(<ArchenemyPanel />);
    expect(container).toBeEmptyDOMElement();
  });

  it("renders the engine-derived active scheme view", () => {
    useGameStore.setState({ gameState: stateWithArchenemy() });

    render(<ArchenemyPanel />);

    expect(screen.getByText("Active scheme")).toBeInTheDocument();
    expect(screen.getByText("Your Puny Minds Cannot Fathom")).toBeInTheDocument();
    expect(screen.getByText("19 in deck")).toBeInTheDocument();
    expect(screen.getByText("2 heroes")).toBeInTheDocument();
  });
});
