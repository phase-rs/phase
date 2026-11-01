import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  ContestPhase,
  ContestResult,
  ContestState,
  EngineAdapter,
  EngineSnapshot,
  GameEvent,
} from "../../../adapter/types.ts";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { useContestStore } from "../../../stores/contestStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { useKeyboardShortcuts } from "../../../hooks/useKeyboardShortcuts.ts";
import { ContestOverlay } from "../ContestOverlay.tsx";

const dispatchActionMock = vi.hoisted(() => vi.fn());
vi.mock("../../../game/dispatch", () => ({ dispatchAction: dispatchActionMock }));

/** Mounts the page's real global shortcut handler next to the overlay. */
function GlobalShortcuts() {
  useKeyboardShortcuts();
  return null;
}

function contestState(phase: ContestPhase, overrides: Partial<ContestState> = {}): ContestState {
  return {
    id: 1,
    kind: "Staring",
    phase,
    config: {
      countdown_ms: 3000,
      inactivity_timeout_ms: 2000,
      reconnect_grace_ms: 10000,
      settle_window_ms: 150,
      max_active_ms: 300000,
    },
    contestants: [
      { player: 0, clicks: 0, last_activity_ms: 0, last_seq: null, connection: "Connected" },
      { player: 1, clicks: 0, last_activity_ms: 0, last_seq: null, connection: "Connected" },
    ],
    clock_ms: 1000,
    go_at_ms: phase === "Active" ? 1000 : 4000,
    ...overrides,
  };
}

function showContest(contest: ContestState, adapter: Partial<EngineAdapter> | null) {
  useGameStore.setState({
    gameMode: "ai",
    adapter: adapter as EngineAdapter | null,
    events: [],
    waitingFor: { type: "PhysicalContest", data: { contest, source_id: 7 } },
  });
}

beforeEach(() => {
  useContestStore.getState().reset();
  useMultiplayerStore.setState({ playerNames: new Map([[1, "Alice"]]) });
});

afterEach(() => {
  cleanup();
  useGameStore.setState({ waitingFor: null, adapter: null, events: [] });
});

describe("ContestOverlay keyboard focus", () => {
  beforeEach(() => dispatchActionMock.mockClear());

  it("focuses the first local contest control as the contest opens", () => {
    showContest(contestState("Preparing"), { submitContestClick: vi.fn(() => true) });
    render(<ContestOverlay />);
    expect(screen.getByRole("button", { name: "Wait for GO…" })).toHaveFocus();
  });

  it("gives a spectator's dialog the focus, since it has no control", () => {
    const contest = contestState("Active");
    contest.contestants = [
      { ...contest.contestants[0], player: 5 },
      { ...contest.contestants[1], player: 6 },
    ];
    showContest(contest, { submitContestClick: vi.fn(() => true) });
    render(<ContestOverlay />);
    expect(screen.getByRole("dialog")).toHaveFocus();
  });

  it("restores the prior focus when the contest closes", () => {
    const outside = document.createElement("button");
    document.body.append(outside);
    outside.focus();
    showContest(contestState("Active"), { submitContestClick: vi.fn(() => true) });
    render(<ContestOverlay />);
    expect(outside).not.toHaveFocus();

    act(() => useGameStore.setState({ waitingFor: null }));

    expect(outside).toHaveFocus();
    outside.remove();
  });

  it("brings focus back when it leaves the modal, so the keys never reach the page behind", () => {
    showContest(contestState("Active"), { submitContestClick: vi.fn(() => true) });
    render(<ContestOverlay />);
    const button = screen.getByRole("button", { name: "Keep clicking!" });

    act(() => button.blur());

    expect(button).toHaveFocus();
  });

  it("clicks on Space and Enter from the focused control without passing priority or arming auto-pass", () => {
    const submitContestClick = vi.fn(() => true);
    showContest(contestState("Active"), { submitContestClick });
    render(
      <>
        <GlobalShortcuts />
        <ContestOverlay />
      </>,
    );
    const button = screen.getByRole("button", { name: "Keep clicking!" });
    expect(button).toHaveFocus();

    fireEvent.keyDown(button, { key: " " });
    fireEvent.keyDown(button, { key: "Enter" });

    expect(submitContestClick).toHaveBeenCalledTimes(2);
    expect(dispatchActionMock).not.toHaveBeenCalled();
  });

  it("never lets a document-routed Space or Enter reach the global shortcuts", () => {
    showContest(contestState("Active"), { submitContestClick: vi.fn(() => true) });
    render(
      <>
        <GlobalShortcuts />
        <ContestOverlay />
      </>,
    );

    // Routed from outside the dialog, as it would be with focus on the page behind it.
    fireEvent.keyDown(document.body, { key: " " });
    fireEvent.keyDown(document.body, { key: "Enter" });
    fireEvent.keyDown(window, { key: "Enter" });

    expect(dispatchActionMock).not.toHaveBeenCalled();
  });
});

describe("ContestOverlay", () => {
  it("is a registered, handled waiting state", () => {
    expect(
      isWaitingForHandled({
        type: "PhysicalContest",
        data: { contest: contestState("Pending"), source_id: 1 },
      }),
    ).toBe(true);
  });

  it("names the contest, explains the rule and shows both contestants", () => {
    showContest(contestState("Preparing"), { submitContestClick: vi.fn(() => true) });
    render(<ContestOverlay />);

    expect(screen.getByRole("heading", { name: "Staring Contest" })).toBeInTheDocument();
    expect(screen.getByText(/The first player to stop for 2 seconds loses/)).toBeInTheDocument();
    expect(screen.getByText("You vs. Alice")).toBeInTheDocument();
    expect(screen.getByText("You: 0")).toBeInTheDocument();
    expect(screen.getByText("Alice: 0")).toBeInTheDocument();
  });

  it("uses the breath-holding wording for Mouth to Mouth", () => {
    showContest(contestState("Preparing", { kind: "BreathHolding" }), { submitContestClick: vi.fn(() => true) });
    render(<ContestOverlay />);
    expect(screen.getByRole("heading", { name: "Breath-Holding Contest" })).toBeInTheDocument();
  });

  it("does not register clicks before GO and tells the player to wait", () => {
    const submitContestClick = vi.fn(() => true);
    showContest(contestState("Preparing"), { submitContestClick });
    render(<ContestOverlay />);

    const button = screen.getByRole("button", { name: "Wait for GO…" });
    expect(button).toHaveAttribute("aria-disabled", "true");
    fireEvent.pointerDown(button);
    fireEvent.keyDown(button, { key: " " });
    expect(submitContestClick).not.toHaveBeenCalled();
    expect(screen.getByRole("status")).toHaveTextContent("Get ready…");
  });

  it("submits each pointer press with a strictly increasing sequence number once Active", () => {
    const submitContestClick = vi.fn((_contest: number, _seq: number) => true);
    showContest(contestState("Active"), { submitContestClick });
    render(<ContestOverlay />);

    const button = screen.getByRole("button", { name: "Keep clicking!" });
    fireEvent.pointerDown(button);
    fireEvent.pointerDown(button);
    fireEvent.pointerDown(button);

    expect(submitContestClick.mock.calls.map(([, seq]) => seq)).toEqual([1, 2, 3]);
    expect(screen.getByText("GO! Keep clicking!")).toBeInTheDocument();
  });

  it("continues numbering above what the engine has already confirmed", () => {
    const submitContestClick = vi.fn(() => true);
    const contest = contestState("Active");
    contest.contestants[0].last_seq = 41;
    showContest(contest, { submitContestClick });
    render(<ContestOverlay />);

    fireEvent.pointerDown(screen.getByRole("button", { name: "Keep clicking!" }));
    expect(submitContestClick).toHaveBeenCalledWith(1, 42);
  });

  it("supports keyboard activation, counts a press once and ignores key auto-repeat", () => {
    const submitContestClick = vi.fn(() => true);
    showContest(contestState("Active"), { submitContestClick });
    render(<ContestOverlay />);

    const button = screen.getByRole("button", { name: "Keep clicking!" });
    fireEvent.keyDown(button, { key: " " });
    fireEvent.keyDown(button, { key: " ", repeat: true });
    fireEvent.keyDown(button, { key: "Enter" });
    fireEvent.keyDown(button, { key: "a" });

    expect(submitContestClick).toHaveBeenCalledTimes(2);
  });

  it("counts an assistive-technology activation (click with no pointer or key event)", () => {
    const submitContestClick = vi.fn(() => true);
    showContest(contestState("Active"), { submitContestClick });
    render(<ContestOverlay />);

    const button = screen.getByRole("button", { name: "Keep clicking!" });
    fireEvent.click(button, { detail: 0 });
    // A mouse click is already counted by its pointerdown; its click event must not double-count.
    fireEvent.click(button, { detail: 1 });

    expect(submitContestClick).toHaveBeenCalledTimes(1);
  });

  it("buffers stamped clicks for a local host when no server transport exists", () => {
    const driveContest = vi.fn();
    showContest(contestState("Active"), { driveContest });
    render(<ContestOverlay />);

    fireEvent.pointerDown(screen.getByRole("button", { name: "Keep clicking!" }));

    const [click] = useContestStore.getState().pendingClicks;
    expect(click).toMatchObject({ contest: 1, player: 0, seq: 1 });
    expect(typeof click.at_ms).toBe("number");
  });

  it("shows live counters pushed between full states", () => {
    showContest(contestState("Active"), { submitContestClick: vi.fn(() => true) });
    render(<ContestOverlay />);

    act(() => useContestStore.getState().setProgress(1, [[0, 12], [1, 9]]));

    expect(screen.getByText("You: 12")).toBeInTheDocument();
    expect(screen.getByText("Alice: 9")).toBeInTheDocument();
  });

  it("announces a pause while a seat reconnects and never lets spectators click", () => {
    const contest = contestState("Active", { paused_since_ms: 1500 });
    contest.contestants = [
      { ...contest.contestants[0], player: 5 },
      { ...contest.contestants[1], player: 6 },
    ];
    showContest(contest, { submitContestClick: vi.fn(() => true) });
    render(<ContestOverlay />);

    expect(screen.getByRole("status")).toHaveTextContent("Contest paused while a player reconnects…");
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it.each<[string, ContestResult, string]>([
    ["a win", { Outcome: { Decided: { winner: 0, loser: 1, reason: "Inactivity" } } }, "You won the contest!"],
    ["a loss", { Outcome: { Decided: { winner: 1, loser: 0, reason: "Inactivity" } } }, "You lost the contest."],
    ["a tie", { Outcome: "Tie" }, "It's a tie — nothing happens."],
    ["a cancellation", { Cancelled: "BothDisconnected" }, "The contest was cancelled — nothing happens."],
  ])("announces %s after the engine settles it", (_name, result, text) => {
    useGameStore.setState({ gameMode: "ai", waitingFor: { type: "Priority", data: { player: 0 } } });
    render(<ContestOverlay />);

    const event: GameEvent = {
      type: "PhysicalContestResolved",
      data: { kind: "Staring", participants: [0, 1], result },
    };
    act(() => useGameStore.setState({ events: [event] }));

    expect(screen.getByText(text)).toBeInTheDocument();
  });
});


describe("ContestOverlay contest identity and seating", () => {
  it("addresses each click to the running contest and restarts numbering for the next one", () => {
    const submitContestClick = vi.fn((_contest: number, _seq: number) => true);
    showContest(contestState("Active", { id: 7 }), { submitContestClick });
    const view = render(<ContestOverlay />);
    fireEvent.pointerDown(screen.getByRole("button", { name: "Keep clicking!" }));
    fireEvent.pointerDown(screen.getByRole("button", { name: "Keep clicking!" }));
    expect(submitContestClick.mock.calls).toEqual([
      [7, 1],
      [7, 2],
    ]);

    // A later contest is a new identity: its sequence is its own, not 3.
    act(() => showContest(contestState("Active", { id: 8 }), { submitContestClick }));
    view.rerender(<ContestOverlay />);
    fireEvent.pointerDown(screen.getByRole("button", { name: "Keep clicking!" }));
    expect(submitContestClick).toHaveBeenLastCalledWith(8, 1);
  });

  it("ignores live counters that belong to a different contest", () => {
    showContest(contestState("Active", { id: 2 }), { submitContestClick: vi.fn(() => true) });
    render(<ContestOverlay />);

    act(() => useContestStore.getState().setProgress(1, [[0, 40], [1, 30]]));
    expect(screen.getByText("You: 0")).toBeInTheDocument();

    act(() => useContestStore.getState().setProgress(2, [[0, 3], [1, 2]]));
    expect(screen.getByText("You: 3")).toBeInTheDocument();
  });

  it("does not buffer a locally stamped click when the transport took it (native host)", () => {
    const submitContestClick = vi.fn(() => true);
    const driveContest = vi.fn();
    showContest(contestState("Active"), { submitContestClick, driveContest });
    render(<ContestOverlay />);

    fireEvent.pointerDown(screen.getByRole("button", { name: "Keep clicking!" }));

    expect(submitContestClick).toHaveBeenCalledWith(1, 1);
    expect(useContestStore.getState().pendingClicks).toHaveLength(0);
  });

  it("falls back to a stamped local click when the adapter declines the route (local host)", () => {
    const submitContestClick = vi.fn(() => false);
    const driveContest = vi.fn();
    showContest(contestState("Active"), { submitContestClick, driveContest });
    render(<ContestOverlay />);

    fireEvent.pointerDown(screen.getByRole("button", { name: "Keep clicking!" }));

    expect(useContestStore.getState().pendingClicks).toMatchObject([{ contest: 1, player: 0, seq: 1 }]);
  });

  it("gives each human in hotseat their own control, and clicks as that seat", () => {
    useGameStore.setState({ aiSeatIds: [] });
    showContest(contestState("Active"), { driveContest: vi.fn() });
    useGameStore.setState({ gameMode: "local" });
    useMultiplayerStore.setState({ playerNames: new Map([[0, "Ann"], [1, "Bob"]]) });
    render(<ContestOverlay />);

    const buttons = screen.getAllByRole("button", { name: "Keep clicking!" });
    expect(buttons).toHaveLength(2);
    fireEvent.pointerDown(buttons[0]);
    fireEvent.pointerDown(buttons[1]);
    fireEvent.pointerDown(buttons[1]);

    expect(
      useContestStore.getState().pendingClicks.map((c) => [c.player, c.seq]),
    ).toEqual([
      [0, 1],
      [1, 1],
      [1, 2],
    ]);
  });

  it("gives a solo player against a configured AI seat one control", () => {
    useGameStore.setState({ aiSeatIds: [1] });
    showContest(contestState("Active"), { driveContest: vi.fn() });
    render(<ContestOverlay />);

    expect(screen.getAllByRole("button", { name: "Keep clicking!" })).toHaveLength(1);
  });
});

describe("ContestOverlay local host tick", () => {
  const running = { id: 1, players: [0, 1] as [number, number] };

  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("drives the engine with the configured AI seats, not a guess from who is not me", async () => {
    const driveContest = vi.fn().mockResolvedValue({
      step: "Unchanged",
      events: [],
      log_entries: [],
      contest: running,
    });
    showContest(contestState("Active"), { driveContest });
    useGameStore.setState({ aiSeatIds: [1] });
    render(<ContestOverlay />);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(60);
    });

    expect(driveContest).toHaveBeenCalled();
    expect(driveContest.mock.calls[0][1]).toEqual([1]);
  });

  it("does not commit a snapshot that resolves after the game was reset", async () => {
    let finishSnapshot: (snapshot: unknown) => void = () => {};
    const getSnapshot = vi.fn(
      () => new Promise<EngineSnapshot>((resolve) => (finishSnapshot = resolve as (s: unknown) => void)),
    );
    const driveContest = vi.fn().mockResolvedValue({
      step: "Settled",
      events: [],
      log_entries: [],
      contest: null,
    });
    const commitEngineSnapshot = vi.fn();
    showContest(contestState("Active"), { driveContest, getSnapshot });
    useGameStore.setState({ commitEngineSnapshot, gameSessionGeneration: 1 });
    render(<ContestOverlay />);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(60);
    });
    expect(getSnapshot).toHaveBeenCalled();

    // The session is reset while the snapshot read is still in flight.
    useGameStore.setState({ gameSessionGeneration: 2 });
    await act(async () => {
      finishSnapshot({});
      await Promise.resolve();
    });

    expect(commitEngineSnapshot).not.toHaveBeenCalled();
  });

  it("keeps a buffered click when the engine rejects the drive, and hands it over once", async () => {
    const click = { contest: 1, player: 0, seq: 1, at_ms: 1_020 };
    const driveContest = vi
      .fn()
      .mockRejectedValueOnce(new Error("engine unavailable"))
      .mockResolvedValue({ step: "Unchanged", events: [], log_entries: [], contest: running });
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    showContest(contestState("Active"), { driveContest });
    useContestStore.getState().bufferClick(click);
    render(<ContestOverlay />);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(210);
    });

    const batches = driveContest.mock.calls.map((call) => call[2]);
    expect(batches[0]).toEqual([click]); // the rejected attempt took it
    expect(batches[1]).toEqual([click]); // ...and the next tick still carried it
    expect(batches.slice(2).flat()).toEqual([]); // ...exactly once more, never again
    expect(useContestStore.getState().pendingClicks).toEqual([]);
    consoleError.mockRestore();
  });

  it("keeps an accepted settlement when its snapshot read fails once, and delivers its events exactly once", async () => {
    const resolved: GameEvent = {
      type: "PhysicalContestResolved",
      data: { kind: "Staring", participants: [0, 1], result: { Outcome: "Tie" } },
    };
    const snapshot = {} as EngineSnapshot;
    const getSnapshot = vi
      .fn()
      .mockRejectedValueOnce(new Error("worker restarting"))
      .mockResolvedValue(snapshot);
    // The engine settled on the first drive; from then on it reports NotInContest and
    // could never recreate the resolution events.
    const driveContest = vi
      .fn()
      .mockResolvedValueOnce({ step: "Settled", events: [resolved], log_entries: [], contest: null })
      .mockResolvedValue({ step: "NotInContest", events: [], log_entries: [], contest: null });
    const commitEngineSnapshot = vi.fn();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    showContest(contestState("Active"), { driveContest, getSnapshot });
    useGameStore.setState({ commitEngineSnapshot, gameSessionGeneration: 1 });
    render(<ContestOverlay />);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(300);
    });

    expect(commitEngineSnapshot).toHaveBeenCalledTimes(1);
    expect(commitEngineSnapshot).toHaveBeenCalledWith(snapshot, {
      events: [resolved],
      logEntries: [],
    });
    // The retry came before the engine was driven any further.
    expect(getSnapshot).toHaveBeenCalledTimes(2);
    expect(getSnapshot.mock.invocationCallOrder[1]).toBeLessThan(
      driveContest.mock.invocationCallOrder[1],
    );
    consoleError.mockRestore();
  });

  it("does not commit a transition its adapter already published", async () => {
    const getSnapshot = vi.fn();
    const driveContest = vi.fn().mockResolvedValue({
      step: "Settled",
      events: [],
      log_entries: [],
      contest: null,
      published: true,
    });
    const commitEngineSnapshot = vi.fn();
    showContest(contestState("Active"), { driveContest, getSnapshot });
    useGameStore.setState({ commitEngineSnapshot });
    render(<ContestOverlay />);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(60);
    });

    expect(driveContest).toHaveBeenCalled();
    expect(getSnapshot).not.toHaveBeenCalled();
    expect(commitEngineSnapshot).not.toHaveBeenCalled();
  });

  it("survives a rejected drive call and keeps ticking", async () => {
    const driveContest = vi
      .fn()
      .mockRejectedValueOnce(new Error("engine unavailable"))
      .mockResolvedValue({ step: "Unchanged", events: [], log_entries: [], contest: running });
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    showContest(contestState("Active"), { driveContest });
    render(<ContestOverlay />);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(160);
    });

    expect(driveContest.mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(consoleError).toHaveBeenCalled();
    consoleError.mockRestore();
  });
});
