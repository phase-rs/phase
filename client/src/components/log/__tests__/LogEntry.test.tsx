import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameLogEntry, GameState } from "../../../adapter/types";
import { useGameStore } from "../../../stores/gameStore";
import { useMultiplayerStore } from "../../../stores/multiplayerStore";
import { LogEntry } from "../LogEntry";

function makeEntry(overrides: Partial<GameLogEntry> = {}): GameLogEntry {
  return {
    seq: 0,
    turn: 1,
    phase: "PreCombatMain",
    category: "Action",
    segments: [],
    ...overrides,
  };
}

describe("LogEntry", () => {
  beforeEach(() => {
    act(() => {
      useGameStore.setState({ gameState: null });
      useMultiplayerStore.setState({ playerNames: new Map() });
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders plain text segments", () => {
    const entry = makeEntry({
      segments: [{ type: "Text", value: "Player attacks with" }],
    });
    render(<LogEntry entry={entry} />);
    expect(screen.getByText("Player attacks with")).toBeInTheDocument();
  });

  it("renders card name segments as styled spans when no inspector provided", () => {
    const entry = makeEntry({
      segments: [{ type: "CardName", value: { name: "Lightning Bolt", object_id: 42 } }],
    });
    render(<LogEntry entry={entry} />);
    expect(screen.getByText("Lightning Bolt")).toBeInTheDocument();
  });

  it("renders card name as a clickable button when onInspectObject is provided", async () => {
    const onInspect = vi.fn();
    const entry = makeEntry({
      segments: [{ type: "CardName", value: { name: "Lightning Bolt", object_id: 42 } }],
    });
    render(<LogEntry entry={entry} onInspectObject={onInspect} />);

    await userEvent.click(screen.getByRole("button", { name: "Lightning Bolt" }));
    expect(onInspect).toHaveBeenCalledWith(42);
  });

  it("renders number segments as bold text", () => {
    const entry = makeEntry({
      segments: [{ type: "Number", value: 5 }],
    });
    render(<LogEntry entry={entry} />);
    const el = screen.getByText("5");
    expect(el.tagName).toBe("SPAN");
    expect(el.className).toContain("font-bold");
  });

  it("renders zone segments as italic text", () => {
    const entry = makeEntry({
      segments: [{ type: "Zone", value: "graveyard" }],
    });
    render(<LogEntry entry={entry} />);
    const el = screen.getByText("graveyard");
    expect(el.className).toContain("italic");
  });

  it("renders keyword segments", () => {
    const entry = makeEntry({
      segments: [{ type: "Keyword", value: "flying" }],
    });
    render(<LogEntry entry={entry} />);
    expect(screen.getByText("flying")).toBeInTheDocument();
  });

  it("renders mana segments", () => {
    const entry = makeEntry({
      segments: [{ type: "Mana", value: "{W}" }],
    });
    render(<LogEntry entry={entry} />);
    expect(screen.getByText("{W}")).toBeInTheDocument();
  });

  it("renders player name using display name from multiplayerStore", () => {
    act(() => {
      useMultiplayerStore.setState({ playerNames: new Map([[0, "Alice"]]) });
    });
    const entry = makeEntry({
      segments: [{ type: "PlayerName", value: { player_id: 0 } }],
    });
    render(<LogEntry entry={entry} />);
    expect(screen.getByText("Alice")).toBeInTheDocument();
  });

  it("renders multiple segments in sequence", () => {
    const entry = makeEntry({
      segments: [
        { type: "Text", value: "Player casts " },
        { type: "CardName", value: { name: "Counterspell", object_id: 1 } },
        { type: "Text", value: " targeting " },
        { type: "CardName", value: { name: "Shock", object_id: 2 } },
      ],
    });
    render(<LogEntry entry={entry} />);
    expect(screen.getByText("Player casts")).toBeInTheDocument();
    expect(screen.getByText("Counterspell")).toBeInTheDocument();
    expect(screen.getByText("targeting")).toBeInTheDocument();
    expect(screen.getByText("Shock")).toBeInTheDocument();
  });

  it("uses seat_order from gameStore for player color styling", () => {
    const gameState = {
      seat_order: [1, 0] as number[],
    } as Partial<GameState> as GameState;
    act(() => useGameStore.setState({ gameState }));

    const entry = makeEntry({
      segments: [{ type: "PlayerName", value: { player_id: 0 } }],
    });
    // Just verify it renders without error when seat_order is present
    expect(() => render(<LogEntry entry={entry} />)).not.toThrow();
  });
});
