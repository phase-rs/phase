import { act } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { OpponentHud } from "../OpponentHud.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";

function makePlayer(id: number) {
  return {
    id,
    life: 20,
    poison_counters: 0,
    mana_pool: { mana: [] },
    library: [],
    hand: [],
    graveyard: [],
    has_drawn_this_turn: false,
    lands_played_this_turn: 0,
    turns_taken: 0,
  };
}

function createTwoPlayerState(overrides: Partial<GameState> = {}): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [makePlayer(0), makePlayer(1)],
    priority_player: 0,
    objects: {},
    next_object_id: 1,
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

describe("OpponentHud designations (single-opponent path)", () => {
  beforeEach(() => {
    localStorage.clear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
    usePreferencesStore.setState({ followActiveOpponent: false });
    useUiStore.setState({ focusedOpponent: 1 });
    useGameStore.setState({ gameState: createTwoPlayerState() });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders the crown when the opponent is the monarch", () => {
    act(() => {
      useGameStore.setState({ gameState: createTwoPlayerState({ monarch: 1 }) });
    });
    render(<OpponentHud />);
    expect(screen.getByLabelText("Monarch")).toBeInTheDocument();
  });

  it("does not render the crown when the local player is the monarch", () => {
    act(() => {
      useGameStore.setState({ gameState: createTwoPlayerState({ monarch: 0 }) });
    });
    render(<OpponentHud />);
    expect(screen.queryByLabelText("Monarch")).toBeNull();
  });

  it("renders the initiative badge for the opponent", () => {
    act(() => {
      useGameStore.setState({ gameState: createTwoPlayerState({ initiative: 1 }) });
    });
    render(<OpponentHud />);
    expect(screen.getByLabelText("Initiative")).toBeInTheDocument();
  });

  it("renders the city's blessing badge for the opponent", () => {
    act(() => {
      useGameStore.setState({ gameState: createTwoPlayerState({ city_blessing: [1] }) });
    });
    render(<OpponentHud />);
    expect(screen.getByLabelText("City's Blessing")).toBeInTheDocument();
  });

  it("renders the ring counter at the opponent's level", () => {
    act(() => {
      useGameStore.setState({ gameState: createTwoPlayerState({ ring_level: { "1": 4 } }) });
    });
    render(<OpponentHud />);
    expect(screen.getByLabelText(/the ring tempts you \(level 4\)/i)).toBeInTheDocument();
  });

  it("renders the energy counter for the opponent", () => {
    const gameState = createTwoPlayerState();
    gameState.players[1].energy = 7;
    act(() => {
      useGameStore.setState({ gameState });
    });
    render(<OpponentHud />);
    expect(screen.getByLabelText("7 energy counters")).toBeInTheDocument();
  });

  it("renders the dungeon badge when the opponent is venturing", () => {
    act(() => {
      useGameStore.setState({
        gameState: createTwoPlayerState({
          dungeon_progress: {
            "1": { current_dungeon: "TombOfAnnihilation", current_room: 0, completed: [] },
          },
        }),
      });
    });
    render(<OpponentHud />);
    expect(screen.getByLabelText("Venturing in Tomb, room 1")).toBeInTheDocument();
  });

  it("does not render the dungeon badge when the opponent has only completed dungeons", () => {
    act(() => {
      useGameStore.setState({
        gameState: createTwoPlayerState({
          dungeon_progress: {
            "1": { current_dungeon: null, current_room: 0, completed: ["Undercity"] },
          },
        }),
      });
    });
    render(<OpponentHud />);
    expect(screen.queryByLabelText(/venturing in/i)).toBeNull();
  });

  it("renders no designation badges when none apply", () => {
    render(<OpponentHud />);
    expect(screen.queryByLabelText("Monarch")).toBeNull();
    expect(screen.queryByLabelText("Initiative")).toBeNull();
    expect(screen.queryByLabelText("City's Blessing")).toBeNull();
    expect(screen.queryByLabelText(/venturing in/i)).toBeNull();
    expect(screen.queryByLabelText(/the ring tempts you/i)).toBeNull();
    expect(screen.queryByLabelText(/energy counter/i)).toBeNull();
  });
});

describe("OpponentHud designations (multiplayer tab path)", () => {
  function createFourPlayerState(overrides: Partial<GameState> = {}): GameState {
    return {
      turn_number: 1,
      active_player: 0,
      phase: "PreCombatMain",
      players: [makePlayer(0), makePlayer(1), makePlayer(2), makePlayer(3)].map((p) => ({
        ...p,
        life: 40,
      })),
      priority_player: 0,
      objects: {},
      next_object_id: 1,
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
      seat_order: [0, 1, 2, 3],
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
      eliminated_players: [],
      ...overrides,
    };
  }

  beforeEach(() => {
    localStorage.clear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
    usePreferencesStore.setState({ followActiveOpponent: false });
    useUiStore.setState({ focusedOpponent: 1 });
    useGameStore.setState({ gameState: createFourPlayerState() });
  });

  afterEach(() => {
    cleanup();
  });

  it("shows the monarch crown on the seat that holds the designation", () => {
    act(() => {
      useGameStore.setState({ gameState: createFourPlayerState({ monarch: 2 }) });
    });
    render(<OpponentHud />);
    // In a 3-opponent tab layout there is exactly one monarch crown,
    // attached to the seat=2 tab.
    const crowns = screen.queryAllByLabelText("Monarch");
    expect(crowns).toHaveLength(1);
  });

  it("does not show any designation when none apply across opponents", () => {
    render(<OpponentHud />);
    expect(screen.queryByLabelText("Monarch")).toBeNull();
    expect(screen.queryByLabelText("Initiative")).toBeNull();
    expect(screen.queryByLabelText("City's Blessing")).toBeNull();
  });
});
