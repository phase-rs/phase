import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, WaitingFor } from "../../../adapter/types";
import { dispatchAction, dispatchResolveAll } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore";
import { DRAFT_BOT_AI_SEAT, useMultiplayerDraftStore } from "../../../stores/multiplayerDraftStore";
import { useMultiplayerStore } from "../../../stores/multiplayerStore";
import { useUiStore } from "../../../stores/uiStore";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
  buildStackEntry,
} from "../../../test/factories/gameStateFactory.ts";
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

function priorityPrompt(player = 0): WaitingFor {
  return buildPriorityWaitingFor({ data: { player } });
}

function spellStackEntry(controller = 0) {
  return buildStackEntry({
    id: 1,
    source_id: 1,
    controller,
    kind: { type: "Spell", data: { card_id: 1 } },
  });
}

function createGameState(waitingFor: WaitingFor): GameState {
  return buildGameState({
    turn_number: 4,
    active_player: 1,
    phase: "DeclareBlockers",
    players: buildPlayers([{ id: 0, turns_taken: 2 }, { id: 1, turns_taken: 2 }]),
    priority_player: 0,
    next_object_id: 201,
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
    auto_pass: { 0: { type: "UntilTurnBoundary", until: "EndOfCurrentTurn" } },
  });
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
    useMultiplayerDraftStore.setState({ matchPairing: null });
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
        ...createGameState(priorityPrompt()),
        turn_decision_controller: 1,
        active_player: 0,
        stack: [spellStackEntry()],
      },
      waitingFor: priorityPrompt(),
      legalActions: [],
    });
    useMultiplayerStore.setState({ activePlayerId: 1, actionPending: false });

    render(<ActionButton />);

    expect(screen.getByRole("button", { name: "Resolve" })).toBeInTheDocument();
  });

  it("disables resolve controls while Resolve All is draining", () => {
    useGameStore.setState({
      gameMode: "online",
      gameState: {
        ...createGameState(priorityPrompt()),
        phase: "PostCombatMain",
        auto_pass: {},
        stack: [spellStackEntry()],
      },
      waitingFor: priorityPrompt(),
      legalActions: [],
      isResolvingAll: true,
    });
    useMultiplayerStore.setState({ activePlayerId: 0, actionPending: false });

    render(<ActionButton />);

    expect(screen.getByRole("button", { name: "Resolve" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Resolve All" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Resolve All" })).toHaveAttribute("aria-busy", "true");
  });

  it("passes an empty AI-seat list in local hotseat so Resolve All auto-yields instead of AI-driving human seats (#4978)", () => {
    useGameStore.setState({
      gameMode: "local",
      gameState: {
        ...createGameState(priorityPrompt()),
        phase: "PostCombatMain",
        auto_pass: {},
        stack: [spellStackEntry()],
      },
      waitingFor: priorityPrompt(),
      legalActions: [],
    });

    render(<ActionButton />);

    fireEvent.click(screen.getByRole("button", { name: /^Resolve All/ }));
    expect(vi.mocked(dispatchResolveAll)).toHaveBeenLastCalledWith(0, []);
  });

  it("builds the AI seat list for Resolve All when the other seats are AI-driven", () => {
    useGameStore.setState({
      gameMode: "ai",
      gameState: {
        ...createGameState(priorityPrompt()),
        phase: "PostCombatMain",
        auto_pass: {},
        stack: [spellStackEntry()],
      },
      waitingFor: priorityPrompt(),
      legalActions: [],
    });

    render(<ActionButton />);

    fireEvent.click(screen.getByRole("button", { name: /^Resolve All/ }));
    expect(vi.mocked(dispatchResolveAll)).toHaveBeenLastCalledWith(0, [
      { playerId: 1, difficulty: "Medium" },
    ]);
  });

  it("uses the live controller's bot seat binding for a Bot draft match", () => {
    useGameStore.setState({
      gameMode: "draft-match",
      gameState: {
        ...createGameState(priorityPrompt()),
        phase: "PostCombatMain",
        auto_pass: {},
        stack: [spellStackEntry()],
      },
      waitingFor: priorityPrompt(),
      legalActions: [],
    });
    useMultiplayerDraftStore.setState({ matchPairing: { type: "Bot" } as never });

    render(<ActionButton />);

    fireEvent.click(screen.getByRole("button", { name: /^Resolve All/ }));
    expect(vi.mocked(dispatchResolveAll)).toHaveBeenLastCalledWith(0, [DRAFT_BOT_AI_SEAT]);
  });

  it("claims no AI seats for a vs-human draft match", () => {
    useGameStore.setState({
      gameMode: "draft-match",
      gameState: {
        ...createGameState(priorityPrompt()),
        phase: "PostCombatMain",
        auto_pass: {},
        stack: [spellStackEntry()],
      },
      waitingFor: priorityPrompt(),
      legalActions: [],
    });
    useMultiplayerDraftStore.setState({ matchPairing: { type: "HumanHost" } as never });

    render(<ActionButton />);

    fireEvent.click(screen.getByRole("button", { name: /^Resolve All/ }));
    expect(vi.mocked(dispatchResolveAll)).toHaveBeenLastCalledWith(0, []);
  });

  it("surfaces an armed UntilStackEmpty session with a cancel affordance while an opponent holds priority", () => {
    useGameStore.setState({
      gameMode: "online",
      gameState: {
        ...createGameState(priorityPrompt(1)),
        phase: "PostCombatMain",
        auto_pass: { 0: { type: "UntilStackEmpty", initial_stack_len: 1 } },
        stack: [spellStackEntry(1)],
      },
      waitingFor: priorityPrompt(1),
      legalActions: [],
      isResolvingAll: false,
    });
    useMultiplayerStore.setState({ activePlayerId: 0, actionPending: false });

    render(<ActionButton />);

    const cancel = screen.getByRole("button", { name: "Resolving Stack..." });
    expect(cancel).toBeEnabled();
    fireEvent.click(cancel);
    expect(vi.mocked(dispatchAction)).toHaveBeenCalledWith({ type: "CancelAutoPass" });
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
