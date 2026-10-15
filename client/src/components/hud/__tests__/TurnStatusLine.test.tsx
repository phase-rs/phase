/**
 * Runtime tests for TurnStatusLine — the persistent "who has priority / why"
 * narration. Renders the real component against real i18n + Zustand stores and
 * asserts on the user-visible English copy, so framing bugs (e.g. "Your
 * priority" shown to a spectator) are caught at the rendered-text level.
 */
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { Phase, StackEntry, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { usePriorityAttentionStore } from "../../../stores/priorityAttentionStore.ts";
import {
  buildGameState,
  buildStackEntry,
} from "../../../test/factories/gameStateFactory.ts";
import { TurnStatusLine } from "../TurnStatusLine.tsx";

function createGameState(o: { active_player?: number; phase?: Phase; stack?: StackEntry[] } = {}) {
  return buildGameState({
    active_player: o.active_player ?? 0,
    phase: o.phase ?? "PreCombatMain",
    priority_player: o.active_player ?? 0,
    next_object_id: 100,
    stack: o.stack ?? [],
    turn_decision_controller: o.active_player ?? 0,
  });
}

const ONE_STACK_ENTRY = [buildStackEntry({ id: 50 })];
const initialVisualPreference = usePreferencesStore.getState().priorityAlertVisual;

function setup(opts: { seat: number; waitingFor: WaitingFor; state?: Parameters<typeof createGameState>[0]; spectate?: boolean }) {
  useGameStore.setState({
    gameMode: opts.spectate ? "spectate" : "online",
    gameState: createGameState(opts.state),
    waitingFor: opts.waitingFor,
  });
  useMultiplayerStore.setState({
    activePlayerId: opts.seat,
    isSpectator: opts.spectate ?? false,
    playerNames: new Map([[1, "Sorin"]]),
  });
}

describe("TurnStatusLine", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false, playerNames: new Map() });
    usePriorityAttentionStore.getState().publish({ cue: "none", chimeCount: 0 });
    usePreferencesStore.setState({ priorityAlertVisual: true });
  });
  afterEach(() => {
    cleanup();
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false, playerNames: new Map() });
    usePriorityAttentionStore.getState().publish({ cue: "none", chimeCount: 0 });
    usePreferencesStore.setState({ priorityAlertVisual: initialVisualPreference });
  });

  it("announces the local player's own priority with the phase reason", () => {
    setup({ seat: 0, waitingFor: { type: "Priority", data: { player: 0 } }, state: { active_player: 0 } });
    render(<TurnStatusLine />);
    const region = screen.getByRole("status");
    expect(region).toHaveTextContent("Your priority");
    // The reason sentence lives in the GameplayTooltip, which now portals to
    // document.body (escaping the card/overlay stacking context), so assert it
    // on the tooltip element rather than as inline text of the status region.
    expect(screen.getByRole("tooltip", { hidden: true })).toHaveTextContent(
      "Your priority — main phase",
    );
    expect(region).toHaveAttribute("aria-live", "polite");
  });

  it("names the opponent we are waiting on, with the stack reason", () => {
    setup({
      seat: 0,
      waitingFor: { type: "Priority", data: { player: 1 } },
      state: { active_player: 0, stack: ONE_STACK_ENTRY },
    });
    render(<TurnStatusLine />);
    expect(screen.getByRole("status")).toHaveTextContent("Waiting for Sorin");
    expect(screen.getByRole("tooltip", { hidden: true })).toHaveTextContent(
      "Waiting for Sorin — responding to the stack",
    );
  });

  it("never frames the decision as the spectator's own", () => {
    setup({ seat: 0, waitingFor: { type: "Priority", data: { player: 0 } }, spectate: true });
    render(<TurnStatusLine />);
    const region = screen.getByRole("status");
    expect(region).not.toHaveTextContent("Your priority");
    expect(region).toHaveTextContent(/Waiting for/);
  });

  it("renders nothing when no decision is pending", () => {
    setup({ seat: 0, waitingFor: { type: "GameOver", data: { winner: 0 } } });
    render(<TurnStatusLine />);
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("tracks pending attention, escalation, dismissal, and the visual preference", () => {
    setup({ seat: 0, waitingFor: { type: "Priority", data: { player: 0 } } });
    render(<TurnStatusLine />);
    const region = screen.getByRole("status");
    expect(region).not.toHaveClass("priority-attention-glow");

    act(() => usePriorityAttentionStore.getState().publish({ cue: "announced", chimeCount: 1 }));
    expect(region).toHaveClass("priority-attention-glow");
    expect(region).not.toHaveClass("priority-attention-glow-escalated");

    act(() => usePriorityAttentionStore.getState().publish({ cue: "reminded", chimeCount: 1 }));
    expect(region).toHaveClass("priority-attention-glow", "priority-attention-glow-escalated");

    act(() => usePreferencesStore.getState().setPriorityAlertVisual(false));
    expect(region).not.toHaveClass("priority-attention-glow");
    expect(region).not.toHaveClass("priority-attention-glow-escalated");

    act(() => usePreferencesStore.getState().setPriorityAlertVisual(true));
    expect(region).toHaveClass("priority-attention-glow", "priority-attention-glow-escalated");

    act(() => usePriorityAttentionStore.getState().publish({ cue: "none", chimeCount: 1 }));
    expect(region).not.toHaveClass("priority-attention-glow");
    expect(region).not.toHaveClass("priority-attention-glow-escalated");
  });

  it.each([
    { player: 1, spectate: false },
    { player: 0, spectate: true },
  ])("never cues another player's decision or a spectator ($player, $spectate)", ({ player, spectate }) => {
    setup({ seat: 0, waitingFor: { type: "Priority", data: { player } }, spectate });
    usePriorityAttentionStore.getState().publish({ cue: "reminded", chimeCount: 1 });
    render(<TurnStatusLine />);
    expect(screen.getByRole("status")).not.toHaveClass("priority-attention-glow");
    expect(screen.getByRole("status")).not.toHaveClass("priority-attention-glow-escalated");
  });
});
