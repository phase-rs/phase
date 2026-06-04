import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, WaitingFor } from "../../../adapter/types.ts";
import { UnhandledWaitingForModal } from "../UnhandledWaitingForModal.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";

function makeState(waitingFor: WaitingFor): GameState {
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
    waiting_for: waitingFor,
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 2,
    eliminated_players: [],
    turn_decision_controller: 0,
  } as unknown as GameState;
}

describe("UnhandledWaitingForModal (issue #311 safety net)", () => {
  beforeEach(() => {
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders nothing when waitingFor type is handled (e.g. Priority)", () => {
    const state = makeState({ type: "Priority", data: { player: 0 } });
    useGameStore.setState({ gameMode: "ai", gameState: state, waitingFor: state.waiting_for });
    const onExit = vi.fn();
    const { container } = render(
      <UnhandledWaitingForModal onExit={onExit} exitLabel="Return to menu" />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("renders nothing when the local player is not the actor", () => {
    // Opponent is acting — local player has nothing to do, no fallback needed.
    const orphan = {
      type: "OrphanEngineChoice",
      data: { player: 1 },
    } as unknown as WaitingFor;
    const state = makeState(orphan);
    useGameStore.setState({ gameMode: "ai", gameState: state, waitingFor: state.waiting_for });
    const onExit = vi.fn();
    const { container } = render(
      <UnhandledWaitingForModal onExit={onExit} exitLabel="Return to menu" />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("surfaces fail-loud diagnostic when local player is the actor on an unhandled type", () => {
    // Engine-only WaitingFor variant that the FE has no modal for.
    const orphan = {
      type: "OrphanEngineChoice",
      data: { player: 0 },
    } as unknown as WaitingFor;
    const state = makeState(orphan);
    useGameStore.setState({ gameMode: "ai", gameState: state, waitingFor: state.waiting_for });
    const onExit = vi.fn();
    render(
      <UnhandledWaitingForModal onExit={onExit} exitLabel="Return to menu" />,
    );
    expect(screen.getByText("Action required, but UI is missing")).toBeInTheDocument();
    // The missing type is named so the user can report it.
    expect(screen.getByText("OrphanEngineChoice")).toBeInTheDocument();
    // Exit button is present and labeled per caller.
    expect(screen.getByRole("button", { name: "Return to menu" })).toBeInTheDocument();
  });

  it("invokes onExit when the exit button is clicked", () => {
    const orphan = {
      type: "OrphanEngineChoice",
      data: { player: 0 },
    } as unknown as WaitingFor;
    const state = makeState(orphan);
    useGameStore.setState({ gameMode: "online", gameState: state, waitingFor: state.waiting_for });
    const onExit = vi.fn();
    render(
      <UnhandledWaitingForModal onExit={onExit} exitLabel="Concede game" />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Concede game" }));
    expect(onExit).toHaveBeenCalledTimes(1);
  });
});
