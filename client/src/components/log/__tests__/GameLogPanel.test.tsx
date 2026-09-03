import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { EngineSnapshot, GameLogEntry } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import {
  buildGameState,
  buildLegalActionsResult,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { GameLogPanel } from "../GameLogPanel.tsx";

const draggable = vi.hoisted(() => ({ ref: { current: null as HTMLDivElement | null } }));

vi.mock("../../../hooks/useDraggableWidget.ts", () => ({
  useDraggableWidget: () => ({
    ref: draggable.ref,
    style: {},
    drag: false,
    dragMomentum: false,
    dragElastic: 0,
    onDragStart: vi.fn(),
    onDragEnd: vi.fn(),
    onClickCapture: undefined,
    dragging: false,
    x: undefined,
    y: undefined,
    scale: 1,
  }),
}));

function entry(
  seq: number,
  text: string,
  overrides: Partial<GameLogEntry> = {},
): GameLogEntry {
  return {
    seq,
    turn: 1,
    phase: "PreCombatMain",
    category: "Stack",
    segments: [{ type: "Text", value: text }],
    presentation: { importance: "Essential", tone: "Informational", boundary: "None", visibility: "Public" },
    ...overrides,
  };
}

function entriesRegion(): HTMLElement {
  return screen.getByRole("region", { name: "Game Log" });
}

function setScrollMetrics(
  element: HTMLElement,
  { clientHeight, scrollHeight, scrollTop }: { clientHeight: number; scrollHeight: number; scrollTop: number },
) {
  Object.defineProperties(element, {
    clientHeight: { configurable: true, value: clientHeight },
    scrollHeight: { configurable: true, value: scrollHeight },
  });
  element.scrollTop = scrollTop;
}

function setViewportWidth(width: number) {
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    writable: true,
    value: width,
  });
  window.dispatchEvent(new Event("resize"));
}

function snapshot(seq: number): EngineSnapshot {
  return {
    state: useGameStore.getState().gameState!,
    legalResult: buildLegalActionsResult(),
    seq,
  };
}

describe("GameLogPanel", () => {
  beforeEach(() => {
    setViewportWidth(1024);
    draggable.ref.current = null;
    useGameStore.getState().reset();
    useGameStore.setState({
      gameState: buildGameState({ waiting_for: buildPriorityWaitingFor() }),
      logHistory: [entry(0, "Initial event")],
    });
    usePreferencesStore.setState({ logPanelLastChoice: "open", logDockSide: "right" });
    useUiStore.setState({
      logPanelOpen: true,
      flexEditMode: false,
      inspectedObjectId: null,
      previewSticky: false,
    });
  });

  afterEach(() => {
    cleanup();
    useGameStore.getState().reset();
    useUiStore.setState({ logPanelOpen: false, flexEditMode: false });
    vi.restoreAllMocks();
  });

  it("follows appended entries only when the reader is at the bottom", () => {
    const requestFrame = vi.fn((callback: FrameRequestCallback) => {
      callback(0);
      return 1;
    });
    vi.stubGlobal("requestAnimationFrame", requestFrame);
    render(<GameLogPanel />);

    const log = entriesRegion();
    setScrollMetrics(log, { clientHeight: 100, scrollHeight: 200, scrollTop: 100 });
    fireEvent.scroll(log);
    setScrollMetrics(log, { clientHeight: 100, scrollHeight: 240, scrollTop: 100 });
    act(() => useGameStore.setState({ logHistory: [entry(0, "Initial event"), entry(1, "Latest event")] }));

    expect(log.scrollTop).toBe(240);
    expect(screen.queryByRole("button", { name: /jump to latest/i })).not.toBeInTheDocument();

    setScrollMetrics(log, { clientHeight: 100, scrollHeight: 240, scrollTop: 24 });
    fireEvent.scroll(log);
    setScrollMetrics(log, { clientHeight: 100, scrollHeight: 280, scrollTop: 24 });
    act(() => useGameStore.setState({ logHistory: [...useGameStore.getState().logHistory, entry(2, "Unread event")] }));

    expect(log.scrollTop).toBe(24);
    expect(screen.getByRole("button", { name: "Jump to latest (1)" })).toBeInTheDocument();
    expect(requestFrame).toHaveBeenCalledOnce();
  });

  it("tracks unread entries after the capped history rolls and clears them for a reset history", () => {
    const cappedHistory = Array.from({ length: 2000 }, (_, sequence) => entry(sequence, `Event ${sequence}`));
    useGameStore.setState({ logHistory: cappedHistory });
    render(<GameLogPanel />);

    const log = entriesRegion();
    setScrollMetrics(log, { clientHeight: 100, scrollHeight: 400, scrollTop: 0 });
    fireEvent.scroll(log);

    act(() => {
      useGameStore.setState({
        logHistory: [...cappedHistory.slice(1), entry(2000, "Capped unread event")],
      });
    });
    expect(screen.getByRole("button", { name: "Jump to latest (1)" })).toBeInTheDocument();

    act(() => useGameStore.getState().commitEngineSnapshot(snapshot(1), { logEntries: [] }));
    expect(screen.getByRole("button", { name: "Jump to latest (1)" })).toBeInTheDocument();

    act(() => {
      useGameStore.getState().commitEngineSnapshot(snapshot(2), {
        extraState: { logHistory: [], nextLogSeq: 0 },
      });
    });
    expect(screen.queryByRole("button", { name: /jump to latest/i })).not.toBeInTheDocument();
  });

  it("does not report an unread count for a trailing boundary without a rendered row", () => {
    render(<GameLogPanel />);

    const log = entriesRegion();
    setScrollMetrics(log, { clientHeight: 100, scrollHeight: 400, scrollTop: 0 });
    fireEvent.scroll(log);
    act(() => {
      useGameStore.setState({
        logHistory: [
          entry(0, "Initial event"),
          entry(1, "Phase changed", {
            category: "Turn",
            phase: "Upkeep",
            presentation: { importance: "Context", tone: "Neutral", boundary: "Phase", visibility: "Public" },
          }),
        ],
      });
    });

    expect(screen.queryByRole("button", { name: /jump to latest/i })).not.toBeInTheDocument();
  });

  it("keeps the scroll position while changing views or filters and exposes active filters", async () => {
    const user = userEvent.setup();
    useGameStore.setState({
      logHistory: [
        entry(0, "Combat event", { category: "Combat" }),
        entry(1, "Life event", { category: "Life" }),
      ],
    });
    render(<GameLogPanel />);

    const log = entriesRegion();
    setScrollMetrics(log, { clientHeight: 100, scrollHeight: 300, scrollTop: 40 });
    await user.click(screen.getByRole("button", { name: "Details" }));
    await user.click(screen.getByRole("button", { name: "Filters (0)" }));
    await user.click(screen.getByRole("button", { name: "Life" }));
    expect(screen.getByRole("button", { name: "Life" })).toHaveAttribute("aria-pressed", "true");
    expect(log.scrollTop).toBe(40);

    await user.type(screen.getByRole("searchbox", { name: "Search game log" }), "missing");
    expect(screen.getByText("No matching events")).toBeInTheDocument();
    expect(log.scrollTop).toBe(40);

    await user.click(within(log).getByRole("button", { name: "Clear filters" }));
    expect(screen.getByRole("searchbox", { name: "Search game log" })).toHaveValue("");
    expect(screen.getByRole("button", { name: "Life" })).toHaveAttribute("aria-pressed", "false");
    expect(screen.getByText("Life event")).toBeInTheDocument();
    expect(log.scrollTop).toBe(40);
  });

  it("renders a selected Turn boundary instead of a no-match state", async () => {
    const user = userEvent.setup();
    useGameStore.setState({
      logHistory: [
        entry(0, "Turn 2", {
          turn: 2,
          phase: "Upkeep",
          category: "Turn",
          presentation: { importance: "Context", tone: "Neutral", boundary: "Turn", visibility: "Public" },
        }),
        entry(1, "Spell cast", { turn: 2 }),
      ],
    });
    render(<GameLogPanel />);

    await user.click(screen.getByRole("button", { name: "Filters (0)" }));
    await user.click(screen.getByRole("button", { name: "Turn" }));

    expect(screen.getByText("T2 · Upkeep")).toBeInTheDocument();
    expect(screen.queryByText("No matching events")).not.toBeInTheDocument();
  });

  it("opens a closed panel when the game ends and can then be dismissed", () => {
    usePreferencesStore.setState({ logPanelLastChoice: "closed" });
    useUiStore.setState({ logPanelOpen: false });
    render(<GameLogPanel />);

    expect(screen.queryByRole("region", { name: "Game log panel" })).not.toBeInTheDocument();

    act(() => {
      useGameStore.setState({
        gameState: buildGameState({ waiting_for: { type: "GameOver", data: { winner: 0 } } }),
      });
    });

    expect(screen.getByRole("region", { name: "Game log panel" })).toBeInTheDocument();
    expect(useUiStore.getState().logPanelOpen).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "Close game log" }));

    expect(useUiStore.getState().logPanelOpen).toBe(false);
  });

  it("shows hidden diagnostic information only after opting in", async () => {
    const user = userEvent.setup();
    useGameStore.setState({
      logHistory: [
        entry(0, "AI draws a card", {
          presentation: { importance: "Detail", tone: "Neutral", boundary: "None", visibility: "HiddenInformation" },
        }),
      ],
    });
    render(<GameLogPanel />);

    await user.click(screen.getByRole("button", { name: "Diagnostics" }));
    expect(screen.queryByText("AI draws a card")).not.toBeInTheDocument();

    await user.click(screen.getByRole("checkbox", { name: "Show hidden information" }));
    expect(screen.getByText("AI draws a card")).toBeInTheDocument();
  });

  it("shares the panel node with drag behavior and stays open through outside interaction", () => {
    render(<GameLogPanel />);

    const panel = screen.getByRole("region", { name: "Game log panel" });
    expect(draggable.ref.current).toBe(panel);

    fireEvent.mouseDown(panel);
    expect(useUiStore.getState().logPanelOpen).toBe(true);
    fireEvent.mouseDown(document.body);
    expect(useUiStore.getState().logPanelOpen).toBe(true);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(useUiStore.getState().logPanelOpen).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "Close game log" }));

    expect(useUiStore.getState().logPanelOpen).toBe(false);
  });

  it("seeds the panel open on desktop from the shipped default", () => {
    // Driven from the store's REAL default rather than a literal "open": with a
    // literal this test would pass against the pre-change seed effect too and so
    // would prove nothing about the shipped default. This way it goes red if the
    // default ever flips back to "closed".
    usePreferencesStore.setState({
      logPanelLastChoice: usePreferencesStore.getInitialState().logPanelLastChoice,
    });
    useUiStore.setState({ logPanelOpen: false });

    render(<GameLogPanel />);

    expect(screen.getByRole("region", { name: "Game log panel" })).toBeInTheDocument();
    expect(useUiStore.getState().logPanelOpen).toBe(true);
  });

  it("never seeds the panel open on a mobile viewport", () => {
    setViewportWidth(800);
    usePreferencesStore.setState({ logPanelLastChoice: "open" });
    useUiStore.setState({ logPanelOpen: false });

    render(<GameLogPanel />);

    expect(screen.queryByRole("region", { name: "Game log panel" })).not.toBeInTheDocument();
    expect(useUiStore.getState().logPanelOpen).toBe(false);
  });

  it("a stale open panel from a previous game does not survive into the next", () => {
    // Exactly the state a previous game's game-over reveal leaves behind: the
    // non-persisted uiStore still says open while the user's remembered choice
    // is closed. The seed is authoritative, so it must close the panel.
    usePreferencesStore.setState({ logPanelLastChoice: "closed" });
    useUiStore.setState({ logPanelOpen: true });

    render(<GameLogPanel />);

    expect(useUiStore.getState().logPanelOpen).toBe(false);
  });

  it("re-applies the remembered choice on a rematch without remounting", () => {
    usePreferencesStore.setState({ logPanelLastChoice: "closed" });
    useUiStore.setState({ logPanelOpen: false });

    render(<GameLogPanel />);

    // Stand in for the game-over reveal, which opens the panel without the user.
    act(() => useUiStore.getState().setLogPanelOpen(true));
    expect(useUiStore.getState().logPanelOpen).toBe(true);

    // A rematch starts a new session in place — the panel never unmounts.
    act(() => {
      useGameStore.setState({
        gameSessionGeneration: useGameStore.getState().gameSessionGeneration + 1,
      });
    });

    expect(useUiStore.getState().logPanelOpen).toBe(false);
  });

  it("does not remember the game-over reveal as a user choice", () => {
    usePreferencesStore.setState({ logPanelLastChoice: "closed" });
    useUiStore.setState({ logPanelOpen: false });

    render(<GameLogPanel />);

    act(() => {
      useGameStore.setState({
        gameState: buildGameState({ waiting_for: { type: "GameOver", data: { winner: 0 } } }),
      });
    });

    expect(useUiStore.getState().logPanelOpen).toBe(true);
    expect(usePreferencesStore.getState().logPanelLastChoice).toBe("closed");
  });

  it("remembers the closed choice when the header close button is clicked", () => {
    render(<GameLogPanel />);

    fireEvent.click(screen.getByRole("button", { name: "Close game log" }));

    expect(useUiStore.getState().logPanelOpen).toBe(false);
    expect(usePreferencesStore.getState().logPanelLastChoice).toBe("closed");
  });

  it("persists a desktop dock-side swap and reserves the matching game rail", () => {
    render(<GameLogPanel />);

    const panel = screen.getByRole("region", { name: "Game log panel" });
    expect(panel).toHaveClass("right-0", "border-l");
    expect(document.documentElement.style.getPropertyValue("--game-right-rail-offset")).toBe("320px");
    expect(document.documentElement.style.getPropertyValue("--game-left-rail-offset")).toBe("0px");

    fireEvent.click(screen.getByRole("button", { name: "Dock game log on the left" }));

    expect(usePreferencesStore.getState().logDockSide).toBe("left");
    expect(panel).toHaveClass("left-0", "border-r");
    expect(document.documentElement.style.getPropertyValue("--game-right-rail-offset")).toBe("0px");
    expect(document.documentElement.style.getPropertyValue("--game-left-rail-offset")).toBe("320px");
  });

  it("keeps the narrow layout right-docked with its existing responsive width", () => {
    setViewportWidth(800);
    usePreferencesStore.setState({ logDockSide: "left" });

    render(<GameLogPanel />);
    // The session seed closes the panel on this mobile viewport. AnimatePresence
    // would keep the exiting node queryable, so asserting on it would be green by
    // accident; reopen explicitly so the assertions read a settled panel.
    act(() => {
      useUiStore.getState().setLogPanelOpen(true);
    });

    const panel = screen.getByRole("region", { name: "Game log panel" });
    expect(panel).toHaveClass("right-0", "border-l", "w-[min(20rem,100vw)]");
    expect(screen.queryByRole("button", { name: /dock game log/i })).not.toBeInTheDocument();
    expect(document.documentElement.style.getPropertyValue("--game-right-rail-offset")).toBe("0px");
    expect(document.documentElement.style.getPropertyValue("--game-left-rail-offset")).toBe("0px");
  });

  it("clears both game-rail offsets when the panel unmounts", () => {
    const { unmount } = render(<GameLogPanel />);

    expect(document.documentElement.style.getPropertyValue("--game-right-rail-offset")).toBe("320px");
    unmount();

    expect(document.documentElement.style.getPropertyValue("--game-right-rail-offset")).toBe("0px");
    expect(document.documentElement.style.getPropertyValue("--game-left-rail-offset")).toBe("0px");
  });

  it("opens a sticky preview when a card-name link is clicked", () => {
    useGameStore.setState({
      logHistory: [
        entry(0, "", {
          segments: [{ type: "CardName", value: { object_id: 42, name: "Pithing Needle" } }],
        }),
      ],
    });
    render(<GameLogPanel />);

    fireEvent.click(screen.getByRole("button", { name: "Pithing Needle" }));

    expect(useUiStore.getState()).toMatchObject({
      inspectedObjectId: 42,
      previewSticky: true,
    });
  });

  it("copies filtered entries with their translated context and announces success", async () => {
    const user = userEvent.setup();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    useGameStore.setState({ logHistory: [entry(0, "casts Lightning Bolt")] });
    render(<GameLogPanel />);

    await user.click(screen.getByRole("button", { name: "Copy 1 filtered log entry" }));

    expect(writeText).toHaveBeenCalledWith("Turn 1 · Main Phase 1 · Stack: casts Lightning Bolt");
    expect(await screen.findByText("Log copied to clipboard")).toBeInTheDocument();
  });
});
