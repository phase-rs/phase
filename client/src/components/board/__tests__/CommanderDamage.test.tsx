import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { CommanderDamage } from "../CommanderDamage.tsx";

/**
 * CR 903.10a: Commander damage tallies are public game state. Every viewer
 * MUST be able to see how much commander damage every player has taken from
 * every commander — including opponents. The engine emits
 * `derived.commander_damage_by_attacker` keyed by the attacking commander's
 * controller, with `victim`, `commander`, and `damage` on each entry, and
 * filters nothing per viewer (`crates/engine/src/game/derived_views.rs`).
 */

function commanderObject(overrides: Partial<GameObject> = {}): GameObject {
  return {
    id: 101,
    card_id: 201,
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
    name: "My Commander",
    power: 5,
    toughness: 5,
    loyalty: null,
    card_types: { supertypes: ["Legendary"], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "Cost", shards: ["Green"], generic: 2 },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: ["Green"],
    base_power: 5,
    base_toughness: 5,
    base_keywords: [],
    base_color: ["Green"],
    timestamp: 1,
    entered_battlefield_turn: null,
    is_commander: true,
    commander_tax: 0,
    ...overrides,
  };
}

function baseGameState(overrides: Partial<GameState> = {}): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 0,
    objects: {},
    next_object_id: 1000,
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
      uses_commander: true,

      allow_debug_actions: false,
    },
    command_zone: [],
    commander_damage: [],
    ...overrides,
  };
}

describe("CommanderDamage", () => {
  afterEach(() => {
    cleanup();
  });

  beforeEach(() => {
    useGameStore.setState({ gameState: undefined, legalActions: [], spellCosts: {} });
    useMultiplayerStore.setState({ playerNames: new Map() });
  });

  /**
   * Scenario: local P0 has dealt 7 commander damage to opponent P1. The
   * derived view keys the entry by the attacker's controller (P0). When
   * rendering opponent P1's panel, CommanderDamage must surface the badge
   * — this is the bug-report scenario.
   */
  it("renders opponent's commander-damage tally taken from the local player", () => {
    const myCmd = commanderObject({ id: 101, owner: 0, controller: 0, name: "My Commander" });
    useGameStore.setState({
      gameState: baseGameState({
        objects: { [myCmd.id]: myCmd },
        command_zone: [myCmd.id],
        derived: {
          commander_damage_by_attacker: {
            // Attacker = local P0; victim = opponent P1.
            "0": [{ victim: 1, commander: myCmd.id, damage: 7 }],
          },
        },
      }),
    });

    render(<CommanderDamage playerId={1} />);

    const root = screen.getByTestId("commander-damage-1");
    expect(root).toBeInTheDocument();
    expect(root.textContent).toContain("My Commander");
    expect(root.textContent).toContain("7");
  });

  /**
   * Mirror case: opponent's commander dealt damage to me. The component
   * already exercises this when rendering the local player's panel; pin
   * the behavior so future refactors can't silently regress it.
   */
  it("renders local player's commander-damage tally taken from an opponent", () => {
    const oppCmd = commanderObject({ id: 202, owner: 1, controller: 1, name: "Opp Commander" });
    useGameStore.setState({
      gameState: baseGameState({
        objects: { [oppCmd.id]: oppCmd },
        command_zone: [oppCmd.id],
        derived: {
          commander_damage_by_attacker: {
            "1": [{ victim: 0, commander: oppCmd.id, damage: 11 }],
          },
        },
      }),
    });

    render(<CommanderDamage playerId={0} />);

    const root = screen.getByTestId("commander-damage-0");
    expect(root.textContent).toContain("Opp Commander");
    expect(root.textContent).toContain("11");
  });

  it("keeps opposing player identity in the tooltip and seat-color rail", () => {
    const oppCmd = commanderObject({ id: 202, owner: 1, controller: 1, name: "Opp Commander" });
    useMultiplayerStore.setState({ playerNames: new Map([[1, "Atraxa"]]) });
    useGameStore.setState({
      gameState: baseGameState({
        seat_order: [0, 1],
        objects: { [oppCmd.id]: oppCmd },
        command_zone: [oppCmd.id],
        derived: {
          commander_damage_by_attacker: {
            "1": [{ victim: 0, commander: oppCmd.id, damage: 11 }],
          },
        },
      }),
    });

    render(<CommanderDamage playerId={0} />);

    const attackerRail = screen.getByTitle("Commander damage from Atraxa: 11/21");
    expect(attackerRail).toHaveStyle({ borderLeftColor: "#F43F5E" });
    expect(screen.queryByText("Atraxa")).not.toBeInTheDocument();
    expect(screen.queryByText("Opp 1")).not.toBeInTheDocument();
  });

  it("shows the player label once when multiple commanders from that player dealt damage", () => {
    const firstCmd = commanderObject({ id: 202, owner: 1, controller: 1, name: "First Partner" });
    const secondCmd = commanderObject({ id: 303, owner: 1, controller: 1, name: "Second Partner" });
    useMultiplayerStore.setState({ playerNames: new Map([[1, "Partner Player"]]) });
    useGameStore.setState({
      gameState: baseGameState({
        seat_order: [0, 1],
        objects: { [firstCmd.id]: firstCmd, [secondCmd.id]: secondCmd },
        command_zone: [firstCmd.id, secondCmd.id],
        derived: {
          commander_damage_by_attacker: {
            "1": [
              { victim: 0, commander: firstCmd.id, damage: 4 },
              { victim: 0, commander: secondCmd.id, damage: 6 },
            ],
          },
        },
      }),
    });

    render(<CommanderDamage playerId={0} />);

    expect(screen.getByText("Partner Player")).toBeInTheDocument();
    expect(screen.getByText("First Partner")).toBeInTheDocument();
    expect(screen.getByText("Second Partner")).toBeInTheDocument();
  });

  /**
   * No damage to this victim → no badge. Component must not render an
   * empty container that takes up layout space on opponent panels.
   */
  it("renders nothing when no commander damage targets this victim", () => {
    const myCmd = commanderObject({ id: 101, owner: 0, controller: 0 });
    useGameStore.setState({
      gameState: baseGameState({
        objects: { [myCmd.id]: myCmd },
        command_zone: [myCmd.id],
        derived: {
          commander_damage_by_attacker: {
            // Damage exists, but it's against P0 — should be invisible on P1's panel.
            "1": [{ victim: 0, commander: myCmd.id, damage: 4 }],
          },
        },
      }),
    });

    render(<CommanderDamage playerId={1} />);

    expect(screen.queryByTestId("commander-damage-1")).not.toBeInTheDocument();
  });
});
