import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, WaitingFor } from "../../../adapter/types";
import { useGameStore } from "../../../stores/gameStore";
import { useMultiplayerStore } from "../../../stores/multiplayerStore";
import { useUiStore } from "../../../stores/uiStore";
import { ActionButton } from "../ActionButton";

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
  dispatchResolveAll: vi.fn(),
}));

function blockerPrompt(): WaitingFor {
  return {
    type: "DeclareBlockers",
    data: {
      player: 0,
      valid_blocker_ids: [100],
      valid_block_targets: { "100": [200] },
    },
  };
}

function createGameState(waitingFor: WaitingFor): GameState {
  return {
    turn_number: 4,
    active_player: 1,
    phase: "DeclareBlockers",
    players: [
      {
        id: 0,
        life: 20,
        poison_counters: 0,
        mana_pool: { mana: [] },
        library: [],
        hand: [],
        graveyard: [],
        has_drawn_this_turn: false,
        lands_played_this_turn: 0,
        turns_taken: 2,
      },
      {
        id: 1,
        life: 20,
        poison_counters: 0,
        mana_pool: { mana: [] },
        library: [],
        hand: [],
        graveyard: [],
        has_drawn_this_turn: false,
        lands_played_this_turn: 0,
        turns_taken: 2,
      },
    ],
    priority_player: 0,
    objects: {},
    next_object_id: 201,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 42,
    combat: {
      attackers: [{ object_id: 200, defending_player: 0, attack_target: { type: "Player", data: 0 } }],
      blocker_assignments: {},
      blocker_to_attacker: {},
      blockers_declared_by: [],
      pending_blocker_declaration_events: [],
      damage_assignments: {},
      first_strike_done: false,
      damage_step_index: null,
      pending_damage: [],
      regular_damage_done: false,
    },
    waiting_for: waitingFor,
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
    auto_pass: { 0: { type: "UntilEndOfTurn" } },
  };
}

describe("ActionButton", () => {
  beforeEach(() => {
    const waitingFor = blockerPrompt();
    useGameStore.setState({
      gameState: createGameState(waitingFor),
      waitingFor,
      legalActions: [],
      isResolvingAll: false,
    });
    useUiStore.setState({
      combatMode: null,
      selectedAttackers: [],
      blockerAssignments: new Map(),
      combatClickHandler: null,
    });
    useMultiplayerStore.setState({ actionPending: false });
  });

  afterEach(() => {
    cleanup();
  });

  it("keeps blocker controls available while pass-until-end-of-turn is armed", () => {
    render(<ActionButton />);

    expect(screen.getByRole("button", { name: "Block with None" })).toBeInTheDocument();
    expect(screen.queryByText("Auto-Passing to End Step...")).not.toBeInTheDocument();
  });

  it("shows resolve when turn decision controller differs from priority player (issue #1218)", () => {
    useGameStore.setState({
      gameMode: "online",
      gameState: {
        ...createGameState({
          type: "Priority",
          data: { player: 0 },
        }),
        turn_decision_controller: 1,
        active_player: 0,
        stack: [
          {
            id: 1,
            source_id: 1,
            controller: 0,
            kind: { type: "Spell", data: { card_id: 1 } },
          },
        ],
      },
      waitingFor: { type: "Priority", data: { player: 0 } },
      legalActions: [],
    });
    useMultiplayerStore.setState({ activePlayerId: 1, actionPending: false });

    render(<ActionButton />);

    expect(screen.getByRole("button", { name: /^Resolve Pass priority/ })).toBeInTheDocument();
  });

  it("disables resolve controls while Resolve All is draining", () => {
    useGameStore.setState({
      gameMode: "online",
      gameState: {
        ...createGameState({
          type: "Priority",
          data: { player: 0 },
        }),
        phase: "PostCombatMain",
        auto_pass: {},
        stack: [
          {
            id: 1,
            source_id: 1,
            controller: 0,
            kind: { type: "Spell", data: { card_id: 1 } },
          },
        ],
      },
      waitingFor: { type: "Priority", data: { player: 0 } },
      legalActions: [],
      isResolvingAll: true,
    });
    useMultiplayerStore.setState({ activePlayerId: 0, actionPending: false });

    render(<ActionButton />);

    expect(screen.getByRole("button", { name: /^Resolve Pass priority/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: /^Resolve All Keep passing priority/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: /^Resolve All Keep passing priority/ })).toHaveAttribute("aria-busy", "true");
  });

  it("shows blocker controls when turn decision controller differs from blocking player (issue #1199)", () => {
    useGameStore.setState({
      gameMode: "online",
      gameState: createGameState(blockerPrompt()),
      waitingFor: blockerPrompt(),
      legalActions: [],
    });
    useGameStore.setState((state) => ({
      gameState: state.gameState
        ? {
            ...state.gameState,
            turn_decision_controller: 1,
            active_player: 0,
          }
        : state.gameState,
    }));
    useMultiplayerStore.setState({ activePlayerId: 1, actionPending: false });

    render(<ActionButton />);

    expect(screen.getByRole("button", { name: "Block with None" })).toBeInTheDocument();
  });
});
