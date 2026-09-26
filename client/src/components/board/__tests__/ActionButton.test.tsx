import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, WaitingFor } from "../../../adapter/types";
import { dispatchAction, dispatchResolveAll } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore";
import { useMultiplayerDraftStore } from "../../../stores/multiplayerDraftStore";
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

    const { container } = render(<ActionButton />);

    expect(screen.getByRole("button", { name: "Resolve" })).toBeInTheDocument();
    expect(container.querySelector("[data-action-button-panel]")).toHaveAttribute(
      "data-stack-action-layout",
      "true",
    );
  });

  it("renders Pass as an accessible fast-forward icon in the compact action layout", () => {
    const waitingFor = priorityPrompt();
    useGameStore.setState({
      gameState: {
        ...createGameState(waitingFor),
        active_player: 0,
        phase: "PreCombatMain",
        stack: [],
        auto_pass: {},
      },
      waitingFor,
      legalActions: [],
    });
    useMultiplayerStore.setState({ activePlayerId: 0, actionPending: false });

    const { container } = render(<ActionButton />);

    const panel = container.querySelector("[data-action-button-panel]");
    const pass = screen.getByRole("button", { name: "Pass" });
    const nextPhase = screen.getByRole("button", { name: "To Begin Combat" });
    expect(panel).toHaveAttribute("data-compact-pass-layout", "true");
    expect(pass).toHaveAttribute("data-pass-action");
    expect(pass).toHaveAttribute("aria-describedby");
    const passIcon = pass.querySelector("[data-pass-fast-forward-icon]");
    expect(passIcon).toBeInTheDocument();
    expect(passIcon).toHaveAttribute("viewBox", "0 0 24 24");
    expect(passIcon).toHaveClass("block", "h-5", "w-5");
    expect(pass.querySelector(".sr-only")).toHaveTextContent("Pass");
    expect(nextPhase).toHaveAttribute("data-next-phase-action");
    expect(nextPhase.querySelector("[data-advance-label-full]")).toHaveTextContent(
      "To Begin Combat",
    );
    expect(nextPhase.querySelector("[data-advance-label-compact]")).toHaveTextContent(
      "To Combat",
    );

    fireEvent.click(pass);
    expect(vi.mocked(dispatchAction)).toHaveBeenCalledWith({
      type: "SetAutoPass",
      data: { mode: { type: "UntilTurnBoundary", until: "EndOfCurrentTurn" } },
    });
  });

  it("keeps priority actions available when end-of-turn auto-pass pauses for an opponent's stack object", () => {
    useGameStore.setState({
      gameMode: "online",
      gameState: {
        ...createGameState(priorityPrompt()),
        phase: "PostCombatMain",
        active_player: 0,
        stack: [spellStackEntry(1)],
      },
      waitingFor: priorityPrompt(),
      legalActions: [],
    });
    useMultiplayerStore.setState({ activePlayerId: 0, actionPending: false });

    render(<ActionButton />);

    fireEvent.click(screen.getByRole("button", { name: "Resolve" }));
    expect(vi.mocked(dispatchAction)).toHaveBeenCalledWith({ type: "PassPriority" });
    expect(screen.getByRole("button", { name: "Auto-Passing to End Step..." })).toBeInTheDocument();
  });

  it("does not block Resolve All behind client-side drain state", () => {
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
    });
    useMultiplayerStore.setState({ activePlayerId: 0, actionPending: false });

    render(<ActionButton />);

    expect(screen.getByRole("button", { name: "Resolve" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Resolve All" })).toBeEnabled();
  });

  it("starts the same engine consent transaction for local hotseat", () => {
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
    expect(vi.mocked(dispatchResolveAll)).toHaveBeenLastCalledWith(0);
  });

  it("starts the same engine consent transaction for AI games", () => {
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
    expect(vi.mocked(dispatchResolveAll)).toHaveBeenLastCalledWith(0);
  });

  it("leaves native AI Resolve All seat ownership to the server", () => {
    useGameStore.setState({
      gameMode: "native-ai",
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
    expect(vi.mocked(dispatchResolveAll)).toHaveBeenLastCalledWith(0);
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
    expect(vi.mocked(dispatchResolveAll)).toHaveBeenLastCalledWith(0);
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
    expect(vi.mocked(dispatchResolveAll)).toHaveBeenLastCalledWith(0);
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
    });
    useMultiplayerStore.setState({ activePlayerId: 0, actionPending: false });

    render(<ActionButton />);

    const cancel = screen.getByRole("button", { name: "Resolving Stack..." });
    expect(cancel).toBeEnabled();
    fireEvent.click(cancel);
    expect(vi.mocked(dispatchAction)).toHaveBeenCalledWith({ type: "CancelAutoPass" });
  });

  it("keeps UntilStackEmpty cancel-only when the local player holds priority", () => {
    useGameStore.setState({
      gameMode: "online",
      gameState: {
        ...createGameState(priorityPrompt()),
        phase: "PostCombatMain",
        active_player: 0,
        auto_pass: { 0: { type: "UntilStackEmpty", initial_stack_len: 1 } },
        stack: [spellStackEntry(1)],
      },
      waitingFor: priorityPrompt(),
      legalActions: [],
    });
    useMultiplayerStore.setState({ activePlayerId: 0, actionPending: false });

    render(<ActionButton />);

    expect(screen.getByRole("button", { name: "Resolving Stack..." })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Resolve" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Resolve All" })).not.toBeInTheDocument();
  });

  it("does not surface an AI recheck session as a human Resolving Stack control", () => {
    useGameStore.setState({
      gameMode: "ai",
      gameState: {
        ...createGameState(priorityPrompt()),
        phase: "PostCombatMain",
        auto_pass: {
          0: {
            type: "UntilStackEmpty",
            initial_stack_len: 1,
            policy: "RecheckNoMeaningfulPriorityAction",
          },
        },
        stack: [spellStackEntry()],
      },
      waitingFor: priorityPrompt(),
      legalActions: [],
    });

    render(<ActionButton />);

    expect(screen.getByRole("button", { name: "Resolve" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Resolving Stack..." })).not.toBeInTheDocument();
  });

  it("no longer client-gates Confirm/Skip on a must-attack creature (engine is the authority)", () => {
    const target = { type: "Player", data: 1 } as const;
    const wf: WaitingFor = {
      type: "DeclareAttackers",
      data: {
        player: 0,
        valid_attacker_ids: [100],
        valid_attack_targets: [target],
        valid_attack_targets_by_attacker: { "100": [target] },
        attacker_constraints: { "100": { kind: "MustAttack", defenders: [] } },
      },
    };
    useGameStore.setState({
      gameState: { ...createGameState(wf), phase: "DeclareAttackers", active_player: 0, auto_pass: {} },
      waitingFor: wf,
      legalActions: [],
    });
    useUiStore.setState({ selectedAttackers: [], blockerAssignments: new Map() });

    render(<ActionButton />);
    // Discriminating: the old build DISABLED "Attack with None" whenever a
    // must-attack creature was unselected. The engine now rejects illegal
    // submissions, so the client must NOT veto — the button stays enabled.
    expect(screen.getByRole("button", { name: "Attack with None" })).toBeEnabled();

    // Selecting the creature enables Confirm, which dispatches the exact engine
    // action shape with the engine-provided target (no client default target).
    act(() => {
      useUiStore.setState({ selectedAttackers: [100] });
    });
    const confirm = screen.getByRole("button", { name: "Confirm Attackers (1)" });
    expect(confirm).toBeEnabled();
    fireEvent.click(confirm);
    expect(vi.mocked(dispatchAction)).toHaveBeenCalledWith({
      type: "DeclareAttackers",
      data: { attacks: [[100, target]] },
    });
  });

  it("does not client-gate Block with None on an unassigned must-block creature", () => {
    const wf: WaitingFor = {
      type: "DeclareBlockers",
      data: {
        player: 0,
        valid_blocker_ids: [100],
        valid_block_targets: { "100": [200] },
        blocker_constraints: { "100": { kind: "MustBlock" } },
      },
    };
    useGameStore.setState({ gameState: createGameState(wf), waitingFor: wf, legalActions: [] });
    useUiStore.setState({ selectedAttackers: [], blockerAssignments: new Map() });

    render(<ActionButton />);
    expect(screen.getByRole("button", { name: "Block with None" })).toBeEnabled();
  });

  it("submits every selected blocker pair without client-side requirement gating", () => {
    const wf: WaitingFor = {
      type: "DeclareBlockers",
      data: {
        player: 0,
        valid_blocker_ids: [100],
        valid_block_targets: { "100": [200, 201] },
        blocker_constraints: { "100": { kind: "MustBlock" } },
      },
    };
    useGameStore.setState({ gameState: createGameState(wf), waitingFor: wf, legalActions: [] });
    useUiStore.setState({
      selectedAttackers: [],
      blockerAssignments: new Map([[100, new Set([200, 201])]]),
    });

    render(<ActionButton />);
    const confirm = screen.getByRole("button", { name: "Confirm Blockers (2)" });
    expect(confirm).toBeEnabled();
    fireEvent.click(confirm);
    expect(vi.mocked(dispatchAction)).toHaveBeenCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 200], [100, 201]] },
    });
  });

  it("clears a pending blocker when the engine supplies a new declaration prompt", () => {
    render(<ActionButton />);

    act(() => useUiStore.getState().combatClickHandler?.(100));
    expect(screen.getByText("Select the attacker this blocker should defend against")).toBeInTheDocument();

    const nextPrompt = blockerPrompt();
    act(() => {
      useGameStore.setState({
        gameState: createGameState(nextPrompt),
        waitingFor: nextPrompt,
      });
    });

    expect(screen.queryByText("Select the attacker this blocker should defend against")).not.toBeInTheDocument();
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
