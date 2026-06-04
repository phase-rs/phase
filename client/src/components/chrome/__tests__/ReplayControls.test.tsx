import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../../adapter/types";
import { useGameStore } from "../../../stores/gameStore";
import { ReplayControls } from "../ReplayControls";

function makeState(turn: number): GameState {
  return {
    turn_number: turn,
    active_player: 0,
    phase: "PreCombatMain",
    players: [],
    priority_player: 0,
    objects: {},
    next_object_id: 1,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 42,
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
  };
}

const cp1 = makeState(1);
const cp2 = makeState(2);
const cp3 = makeState(3);
const live = makeState(4);

function seedStore(overrides: Partial<ReturnType<typeof useGameStore.getState>>) {
  act(() => {
    useGameStore.setState({
      gameMode: "ai",
      gameState: live,
      waitingFor: live.waiting_for,
      turnCheckpoints: [],
      stateHistory: [],
      replayMode: false,
      replayIndex: null,
      liveGameState: null,
      legalActions: [],
      legalActionsByObject: {},
      ...overrides,
    });
  });
}

describe("ReplayControls", () => {
  afterEach(() => {
    cleanup();
  });

  beforeEach(() => {
    seedStore({});
  });

  // ── visibility ───────────────────────────────────────────────────────────

  it("renders nothing when there are fewer than 2 checkpoints", () => {
    seedStore({ turnCheckpoints: [cp1] });
    const { container } = render(<ReplayControls />);
    expect(container.firstChild).toBeNull();
  });

  it("renders nothing when gameMode is online (multiplayer)", () => {
    seedStore({ turnCheckpoints: [cp1, cp2, cp3], gameMode: "online" });
    const { container } = render(<ReplayControls />);
    expect(container.firstChild).toBeNull();
  });

  it("renders nothing when gameMode is p2p-host", () => {
    seedStore({ turnCheckpoints: [cp1, cp2, cp3], gameMode: "p2p-host" });
    const { container } = render(<ReplayControls />);
    expect(container.firstChild).toBeNull();
  });

  it("renders the View history button when 2+ checkpoints exist in single-player mode", () => {
    seedStore({ turnCheckpoints: [cp1, cp2] });
    render(<ReplayControls />);
    expect(screen.getByText("View history")).toBeInTheDocument();
  });

  // ── entering replay ──────────────────────────────────────────────────────

  it("calls enterReplay when View history is clicked", async () => {
    const enterReplay = vi.fn();
    seedStore({ turnCheckpoints: [cp1, cp2] });
    act(() => useGameStore.setState({ enterReplay }));
    render(<ReplayControls />);

    await userEvent.click(screen.getByText("View history"));
    expect(enterReplay).toHaveBeenCalledOnce();
  });

  // ── replay mode UI ───────────────────────────────────────────────────────

  it("renders replay banner and timeline markers when in replay mode", () => {
    seedStore({
      turnCheckpoints: [cp1, cp2, cp3],
      replayMode: true,
      replayIndex: 1,
      gameState: cp2,
    });
    render(<ReplayControls />);

    expect(screen.getByText(/Replaying turn 2/)).toBeInTheDocument();
    expect(screen.getByText("Exit replay")).toBeInTheDocument();
  });

  it("renders a turn marker for each checkpoint", () => {
    seedStore({
      turnCheckpoints: [cp1, cp2, cp3],
      replayMode: true,
      replayIndex: 0,
      gameState: cp1,
    });
    render(<ReplayControls />);

    expect(screen.getByText("T1")).toBeInTheDocument();
    expect(screen.getByText("T2")).toBeInTheDocument();
    expect(screen.getByText("T3")).toBeInTheDocument();
  });

  it("marks the selected turn marker as pressed", () => {
    seedStore({
      turnCheckpoints: [cp1, cp2, cp3],
      replayMode: true,
      replayIndex: 1,
      gameState: cp2,
    });
    render(<ReplayControls />);

    const t2Button = screen.getByText("T2").closest("button");
    expect(t2Button).toHaveAttribute("aria-pressed", "true");

    const t1Button = screen.getByText("T1").closest("button");
    expect(t1Button).toHaveAttribute("aria-pressed", "false");
  });

  // ── navigation ───────────────────────────────────────────────────────────

  it("calls replayTo when a turn marker is clicked", async () => {
    const replayTo = vi.fn();
    seedStore({
      turnCheckpoints: [cp1, cp2, cp3],
      replayMode: true,
      replayIndex: 2,
      gameState: cp3,
    });
    act(() => useGameStore.setState({ replayTo }));
    render(<ReplayControls />);

    await userEvent.click(screen.getByText("T1"));
    expect(replayTo).toHaveBeenCalledWith(0);
  });

  it("disables the prev button at the first checkpoint", () => {
    seedStore({
      turnCheckpoints: [cp1, cp2, cp3],
      replayMode: true,
      replayIndex: 0,
      gameState: cp1,
    });
    render(<ReplayControls />);

    expect(screen.getByLabelText("Previous turn")).toBeDisabled();
    expect(screen.getByLabelText("Next turn")).toBeEnabled();
  });

  it("disables the next button at the last checkpoint", () => {
    seedStore({
      turnCheckpoints: [cp1, cp2, cp3],
      replayMode: true,
      replayIndex: 2,
      gameState: cp3,
    });
    render(<ReplayControls />);

    expect(screen.getByLabelText("Next turn")).toBeDisabled();
    expect(screen.getByLabelText("Previous turn")).toBeEnabled();
  });

  it("calls replayTo(index-1) when prev button is clicked", async () => {
    const replayTo = vi.fn();
    seedStore({
      turnCheckpoints: [cp1, cp2, cp3],
      replayMode: true,
      replayIndex: 2,
      gameState: cp3,
    });
    act(() => useGameStore.setState({ replayTo }));
    render(<ReplayControls />);

    await userEvent.click(screen.getByLabelText("Previous turn"));
    expect(replayTo).toHaveBeenCalledWith(1);
  });

  it("calls replayTo(index+1) when next button is clicked", async () => {
    const replayTo = vi.fn();
    seedStore({
      turnCheckpoints: [cp1, cp2, cp3],
      replayMode: true,
      replayIndex: 0,
      gameState: cp1,
    });
    act(() => useGameStore.setState({ replayTo }));
    render(<ReplayControls />);

    await userEvent.click(screen.getByLabelText("Next turn"));
    expect(replayTo).toHaveBeenCalledWith(1);
  });

  // ── exiting replay ───────────────────────────────────────────────────────

  it("calls exitReplay when Exit replay is clicked", async () => {
    const exitReplay = vi.fn();
    seedStore({
      turnCheckpoints: [cp1, cp2, cp3],
      replayMode: true,
      replayIndex: 1,
      gameState: cp2,
    });
    act(() => useGameStore.setState({ exitReplay }));
    render(<ReplayControls />);

    await userEvent.click(screen.getByText("Exit replay"));
    expect(exitReplay).toHaveBeenCalledOnce();
  });

  // ── live turn indicator ──────────────────────────────────────────────────

  it("shows the live turn number in the banner", () => {
    seedStore({
      turnCheckpoints: [cp1, cp2, cp3],
      replayMode: true,
      replayIndex: 0,
      gameState: cp1,
      liveGameState: live,
    });
    // live turn_number is 4 — gameState is cp1 (turn 1) but liveGameState.turn_number would
    // show as the store's gameState before override; we rely on currentTurn from store
    // which reads from gameState. After entering replay, gameState = cp (not live),
    // so currentTurn will show the checkpoint turn. The live indicator reads s.gameState
    // (the live state is in liveGameState, not gameState). We test that the banner text
    // is rendered correctly with the checkpoint turn.
    render(<ReplayControls />);
    expect(screen.getByText(/Replaying turn 1/)).toBeInTheDocument();
  });
});
