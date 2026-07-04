import { act } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { PlayerHud } from "../PlayerHud.tsx";

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

describe("PlayerHud designations", () => {
  beforeEach(() => {
    useMultiplayerStore.setState({ activePlayerId: 0 });
    useGameStore.setState({ gameState: createGameState() });
  });

  afterEach(() => {
    cleanup();
  });

  describe("Monarch", () => {
    it("renders the crown when the local player is the monarch", () => {
      act(() => {
        useGameStore.setState({ gameState: createGameState({ monarch: 0 }) });
      });
      render(<PlayerHud />);
      expect(screen.getByLabelText("Monarch")).toBeInTheDocument();
    });

    it("does not render the crown when an opponent is the monarch", () => {
      act(() => {
        useGameStore.setState({ gameState: createGameState({ monarch: 1 }) });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText("Monarch")).toBeNull();
    });

    it("does not render the crown when no one is the monarch", () => {
      render(<PlayerHud />);
      expect(screen.queryByLabelText("Monarch")).toBeNull();
    });
  });

  describe("Initiative", () => {
    it("renders when the local player has the initiative", () => {
      act(() => {
        useGameStore.setState({ gameState: createGameState({ initiative: 0 }) });
      });
      render(<PlayerHud />);
      expect(screen.getByLabelText("Initiative")).toBeInTheDocument();
    });

    it("does not render when an opponent has the initiative", () => {
      act(() => {
        useGameStore.setState({ gameState: createGameState({ initiative: 1 }) });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText("Initiative")).toBeNull();
    });

    it("does not render when no one has the initiative", () => {
      render(<PlayerHud />);
      expect(screen.queryByLabelText("Initiative")).toBeNull();
    });
  });

  describe("City's Blessing", () => {
    it("renders when the local player has the blessing", () => {
      act(() => {
        useGameStore.setState({ gameState: createGameState({ city_blessing: [0] }) });
      });
      render(<PlayerHud />);
      expect(screen.getByLabelText("City's Blessing")).toBeInTheDocument();
    });

    it("does not render when only an opponent has the blessing", () => {
      act(() => {
        useGameStore.setState({ gameState: createGameState({ city_blessing: [1] }) });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText("City's Blessing")).toBeNull();
    });

    it("does not render when no one has the blessing", () => {
      render(<PlayerHud />);
      expect(screen.queryByLabelText("City's Blessing")).toBeNull();
    });
  });

  describe("Ring level", () => {
    it("renders the ring counter at level 3 for the local player", () => {
      act(() => {
        useGameStore.setState({ gameState: createGameState({ ring_level: { "0": 3 } }) });
      });
      render(<PlayerHud />);
      expect(screen.getByLabelText(/the ring tempts you \(level 3\)/i)).toBeInTheDocument();
    });

    it("does not render at level 0", () => {
      act(() => {
        useGameStore.setState({ gameState: createGameState({ ring_level: { "0": 0 } }) });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText(/the ring tempts you/i)).toBeNull();
    });

    it("does not render when only an opponent is tempted", () => {
      act(() => {
        useGameStore.setState({ gameState: createGameState({ ring_level: { "1": 2 } }) });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText(/the ring tempts you/i)).toBeNull();
    });
  });

  describe("Energy", () => {
    it("renders the energy counter when the local player has energy", () => {
      const gameState = createGameState();
      gameState.players[0].energy = 5;
      act(() => {
        useGameStore.setState({ gameState });
      });
      render(<PlayerHud />);
      expect(screen.getByLabelText("5 energy counters")).toBeInTheDocument();
    });

    it("uses singular form for one energy", () => {
      const gameState = createGameState();
      gameState.players[0].energy = 1;
      act(() => {
        useGameStore.setState({ gameState });
      });
      render(<PlayerHud />);
      expect(screen.getByLabelText("1 energy counter")).toBeInTheDocument();
    });

    it("does not render at zero energy", () => {
      render(<PlayerHud />);
      expect(screen.queryByLabelText(/energy counter/)).toBeNull();
    });
  });

  describe("Dungeon", () => {
    it("renders the dungeon badge when the local player is venturing", () => {
      act(() => {
        useGameStore.setState({
          gameState: createGameState({
            dungeon_progress: {
              "0": { current_dungeon: "LostMineOfPhandelver", current_room: 1, completed: [] },
            },
          }),
        });
      });
      render(<PlayerHud />);
      expect(screen.getByLabelText("Venturing in Lost Mine, room 2")).toBeInTheDocument();
    });

    it("does not render when the player has progress but no active dungeon", () => {
      act(() => {
        useGameStore.setState({
          gameState: createGameState({
            dungeon_progress: {
              "0": { current_dungeon: null, current_room: 0, completed: ["TombOfAnnihilation"] },
            },
          }),
        });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText(/venturing in/i)).toBeNull();
    });

    it("does not render when only an opponent is venturing", () => {
      act(() => {
        useGameStore.setState({
          gameState: createGameState({
            dungeon_progress: {
              "1": { current_dungeon: "Undercity", current_room: 0, completed: [] },
            },
          }),
        });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText(/venturing in/i)).toBeNull();
    });
  });

  // CR 732.2a: the `∞` HUD badge is driven ONLY by the engine projection
  // `derived.unbounded_resources` — the FE never derives which axes are unbounded.
  describe("Unbounded resources (∞)", () => {
    it("renders an ∞ badge for the local player's engine-attributed axis", () => {
      act(() => {
        useGameStore.setState({
          gameState: createGameState({
            derived: { unbounded_resources: [{ player: 0, axis: "TokensCreated" }] },
          }),
        });
      });
      render(<PlayerHud />);
      // REVERT-PROBE: stop reading `derived.unbounded_resources` (or remove the
      // PlayerHud map) → the badge is absent → this assertion fails.
      expect(screen.getByLabelText("Unbounded tokens (∞)")).toBeInTheDocument();
    });

    it("does not render when there are no unbounded resources", () => {
      act(() => {
        useGameStore.setState({
          gameState: createGameState({ derived: { unbounded_resources: [] } }),
        });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText(/Unbounded/)).toBeNull();
    });

    it("does not render when only an opponent has an unbounded axis", () => {
      act(() => {
        useGameStore.setState({
          gameState: createGameState({
            derived: { unbounded_resources: [{ player: 1, axis: "TokensCreated" }] },
          }),
        });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText(/Unbounded/)).toBeNull();
    });

    it("collapses multiple axes of the same family into one badge", () => {
      act(() => {
        useGameStore.setState({
          gameState: createGameState({
            derived: {
              unbounded_resources: [
                { player: 0, axis: { Mana: "Red" } },
                { player: 0, axis: { Mana: "Blue" } },
              ],
            },
          }),
        });
      });
      render(<PlayerHud />);
      // Six Mana(color) rows would collapse the same way: one mana family badge.
      expect(screen.getAllByLabelText("Unbounded mana (∞)")).toHaveLength(1);
    });
  });
});
