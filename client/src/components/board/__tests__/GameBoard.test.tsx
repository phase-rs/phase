import { cleanup, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { GameBoard } from "../GameBoard.tsx";

vi.mock("../ArchenemyPanel.tsx", () => ({
  ArchenemyPanel: () => null,
}));

vi.mock("../CombatLine.tsx", () => ({
  CombatLine: () => <div data-testid="combat-line" />,
}));

vi.mock("../PlanechasePanel.tsx", () => ({
  PlanechasePanel: () => null,
}));

vi.mock("../PlayerArea.tsx", () => ({
  PlayerArea: ({ playerId }: { playerId: number }) => (
    <div data-testid={`player-area-${playerId}`} />
  ),
}));

vi.mock("../OpponentSeatHeader.tsx", () => ({
  OpponentSeatHeader: ({ playerId }: { playerId: number }) => (
    <div data-testid={`opponent-seat-header-${playerId}`} data-player-hud={String(playerId)} />
  ),
}));

vi.mock("../../flexlayout/DraggableWidget.tsx", () => ({
  DraggableWidget: ({ children }: { children: ReactNode }) => <>{children}</>,
}));

vi.mock("../../hand/OpponentHand.tsx", () => ({
  OpponentHand: ({ playerId }: { playerId?: number }) => (
    <div data-testid={`opponent-hand-${playerId ?? "focused"}`} />
  ),
}));

vi.mock("../../zone/ExilePile.tsx", () => ({
  ExilePile: () => null,
}));

vi.mock("../../zone/GraveyardPile.tsx", () => ({
  GraveyardPile: () => null,
}));

vi.mock("../../zone/LibraryPile.tsx", () => ({
  LibraryPile: () => null,
}));

function createFourPlayerState(): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [0, 1, 2, 3].map((id) => ({
      id,
      life: 40,
      poison_counters: 0,
      mana_pool: { mana: [] },
      library: [],
      hand: [],
      graveyard: [],
      has_drawn_this_turn: false,
      lands_played_this_turn: 0,
      turns_taken: 0,
    })),
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
    seat_order: [0, 1, 2, 3],
    eliminated_players: [],
  } as unknown as GameState;
}

describe("GameBoard multiplayer layout", () => {
  beforeEach(() => {
    useGameStore.setState({
      gameMode: "local",
      gameState: createFourPlayerState(),
      waitingFor: { type: "Priority", data: { player: 0 } },
      legalActionsByObject: {},
    });
    useUiStore.setState({
      focusedOpponent: 2,
      blockerAssignments: new Map(),
    });
    usePreferencesStore.setState({ multiplayerBoardLayout: "focused" });
  });

  afterEach(() => {
    cleanup();
  });

  it("defaults to the focused opponent plus local player", () => {
    render(<GameBoard oppHud={<div />} playerHud={<div />} />);

    expect(screen.getByTestId("player-area-2")).toBeInTheDocument();
    expect(screen.getByTestId("player-area-0")).toBeInTheDocument();
    expect(screen.queryByTestId("player-area-1")).toBeNull();
    expect(screen.queryByTestId("player-area-3")).toBeNull();
  });

  it("renders each live opponent once plus local player in split mode", () => {
    usePreferencesStore.setState({ multiplayerBoardLayout: "split" });

    render(<GameBoard oppHud={<div data-testid="global-opponent-hud" />} playerHud={<div />} />);

    for (const playerId of [0, 1, 2, 3]) {
      expect(screen.getAllByTestId(`player-area-${playerId}`)).toHaveLength(1);
    }
    expect(screen.queryByTestId("global-opponent-hud")).toBeNull();
    expect(screen.getByTestId("opponent-seat-pane-1")).toHaveClass("group/opponent-seat");
    expect(screen.getByTestId("opponent-seat-header-1")).toBeInTheDocument();
    expect(document.querySelector('[data-player-hud="1"]')).toBeTruthy();
    expect(screen.getByTestId("opponent-seat-pane-2")).toHaveClass("group/opponent-seat");
    expect(screen.getByTestId("opponent-seat-header-2")).toBeInTheDocument();
    expect(document.querySelector('[data-player-hud="2"]')).toBeTruthy();
    expect(screen.getByTestId("opponent-seat-pane-3")).toHaveClass("group/opponent-seat");
    expect(screen.getByTestId("opponent-seat-header-3")).toBeInTheDocument();
    expect(document.querySelector('[data-player-hud="3"]')).toBeTruthy();
  });
});
