import { act } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, TargetRef, WaitingFor } from "../../../adapter/types.ts";
import { OpponentHud } from "../OpponentHud.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject } from "../../../test/factories/gameObjectFactory.ts";

function setViewportWidth(width: number) {
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    writable: true,
    value: width,
  });
  window.dispatchEvent(new Event("resize"));
}

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return {
    turn_number: 1,
    active_player: 2,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 2, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 3, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 2,
    objects: {},
    next_object_id: 1,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: { type: "Priority", data: { player: 2 } },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
    seat_order: [0, 1, 2, 3],
    format_config: {
      format: "Commander",
      starting_life: 40,
      min_players: 2,
      max_players: 4,
      deck_size: 100,
      singleton: true,
      command_zone: true,
      commander_damage_threshold: 21,
      range_of_influence: null,
      team_based: false,
      uses_commander: true,

      allow_debug_actions: false,
    },
    eliminated_players: [],
    ...overrides,
  };
}

describe("OpponentHud", () => {
  beforeEach(() => {
    setViewportWidth(1024);
    localStorage.clear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
    usePreferencesStore.setState({ followActiveOpponent: false });
    useUiStore.setState({ focusedOpponent: 1 });
    useGameStore.setState({ gameState: createGameState() });
  });

  afterEach(() => {
    cleanup();
  });

  it("auto-selects the active opponent when Follow is enabled", async () => {
    render(<OpponentHud />);

    fireEvent.click(screen.getByRole("button", { name: /follow active opponent/i }));

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(2);
    });

    act(() => {
      useGameStore.setState({
        gameState: createGameState({ active_player: 3, priority_player: 3, waiting_for: { type: "Priority", data: { player: 3 } } }),
      });
    });

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(3);
    });
  });

  it("disables Follow when manually selecting a non-active opponent", async () => {
    usePreferencesStore.setState({ followActiveOpponent: true });
    render(<OpponentHud />);

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(2);
    });

    fireEvent.click(screen.getByRole("button", { name: /Opp 4/ }));

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(3);
    });
    expect(usePreferencesStore.getState().followActiveOpponent).toBe(false);

    act(() => {
      useGameStore.setState({
        gameState: createGameState({ active_player: 1, priority_player: 1, waiting_for: { type: "Priority", data: { player: 1 } } }),
      });
    });

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(3);
    });
  });

  it("continues allowing manual opponent focus after Follow is disabled by selection", async () => {
    usePreferencesStore.setState({ followActiveOpponent: true });
    render(<OpponentHud />);

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(2);
    });

    fireEvent.click(screen.getByRole("button", { name: /Opp 4/ }));

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(3);
    });
    expect(usePreferencesStore.getState().followActiveOpponent).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: /Opp 2/ }));

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(1);
    });
  });

  it("keeps the Follow toggle usable after selecting the last opponent", async () => {
    usePreferencesStore.setState({ followActiveOpponent: true });
    render(<OpponentHud />);

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(2);
    });

    fireEvent.click(screen.getByRole("button", { name: /Opp 4/ }));

    await waitFor(() => {
      expect(usePreferencesStore.getState().followActiveOpponent).toBe(false);
    });

    fireEvent.click(screen.getByRole("button", { name: /follow active opponent/i }));

    expect(usePreferencesStore.getState().followActiveOpponent).toBe(true);
  });

  it("keeps Follow enabled when selecting the active opponent", async () => {
    usePreferencesStore.setState({ followActiveOpponent: true });
    render(<OpponentHud />);

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(2);
    });

    fireEvent.click(screen.getByRole("button", { name: /Opp 3/ }));

    expect(usePreferencesStore.getState().followActiveOpponent).toBe(true);
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("keeps Follow enabled when browsing opponents on my turn", async () => {
    usePreferencesStore.setState({ followActiveOpponent: true });
    useGameStore.setState({
      gameState: createGameState({
        active_player: 0,
        priority_player: 0,
        waiting_for: { type: "Priority", data: { player: 0 } },
      }),
    });
    render(<OpponentHud />);

    fireEvent.click(screen.getByRole("button", { name: /Opp 4/ }));

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(3);
    });
    expect(usePreferencesStore.getState().followActiveOpponent).toBe(true);
  });

  it("does not override manual selection while Follow is disabled", async () => {
    render(<OpponentHud />);

    fireEvent.click(screen.getByRole("button", { name: /Opp 4/ }));

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(3);
    });

    act(() => {
      useGameStore.setState({
        gameState: createGameState({ active_player: 2, priority_player: 2, waiting_for: { type: "Priority", data: { player: 2 } } }),
      });
    });

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(3);
    });
  });

  it("renders compact poison and speed badges in multiplayer tabs", () => {
    const gameState = createGameState();
    gameState.players[1].poison_counters = 3;
    gameState.players[1].speed = 2;

    act(() => {
      useGameStore.setState({ gameState });
    });

    render(<OpponentHud />);

    expect(screen.getByTitle("Poison counters: 3")).toHaveAttribute("aria-label", "3 poison counters");
    expect(screen.getByTitle("Speed: 2")).toHaveAttribute("aria-label", "Speed 2");
    expect(screen.queryByText("Speed")).toBeNull();
  });

  it("hides zero poison counters", () => {
    render(<OpponentHud />);

    expect(screen.queryByTitle(/Poison counters:/)).toBeNull();
  });

  describe("FFA targeting intent disambiguation", () => {
    // Regression coverage for the Goblin Sharpshooter bug: in a 4-player
    // FFA, clicking an opponent's tab during a target-selection waiting
    // state used to fire `ChooseTarget(Player)` immediately, making the
    // opponent's board unreachable when their player was simultaneously a
    // legal target. The model is now two-step at the whole-tab level:
    // first click on an unfocused tab focuses it (navigate); the second
    // click on the now-focused tab commits the player target (commit).
    function targetSelectionWaitingFor(legalPlayers: number[]): WaitingFor {
      const targets: TargetRef[] = legalPlayers.map((p) => ({ Player: p }));
      // OpponentHud reads `data.player`, `data.selection.current_legal_targets`,
      // and (only for CopyRetarget) `data.target_slots`. The other fields
      // (`pending_cast`, `target_slots`) are required by the TS discriminated
      // union but the renderer never reads them under TargetSelection, so a
      // shallow cast keeps the fixture small.
      return {
        type: "TargetSelection",
        data: {
          player: 0,
          selection: {
            current_slot: 0,
            current_legal_targets: targets,
          },
          target_slots: [{ legal_targets: targets }],
          pending_cast: {} as never,
        },
      } as WaitingFor;
    }

    function mountWithTargeting(legalPlayers: number[] = [1, 2, 3]) {
      const dispatch = vi.fn().mockResolvedValue([]);
      const wf = targetSelectionWaitingFor(legalPlayers);
      useGameStore.setState({ dispatch });
      act(() => {
        useGameStore.setState({
          gameState: createGameState({ waiting_for: wf }),
          waitingFor: wf,
        });
      });
      return { dispatch };
    }

    it("first click on an unfocused targetable tab focuses it (does NOT target)", async () => {
      // Opp 4 is player 3. beforeEach set focus to player 1, so player 3
      // is unfocused at start. First click should focus, not target.
      const { dispatch } = mountWithTargeting();
      render(<OpponentHud />);

      fireEvent.click(screen.getByRole("button", { name: /Opp 4/ }));

      await waitFor(() => {
        expect(useUiStore.getState().focusedOpponent).toBe(3);
      });
      expect(dispatch).not.toHaveBeenCalled();
    });

    it("second click on the focused targetable tab commits the player target", () => {
      const { dispatch } = mountWithTargeting();
      // Pre-focus player 3 so the click is the *second* click (commit step).
      useUiStore.setState({ focusedOpponent: 3 });
      render(<OpponentHud />);

      fireEvent.click(screen.getByRole("button", { name: "Target Opp 4" }));

      expect(dispatch).toHaveBeenCalledWith({
        type: "ChooseTarget",
        data: { target: { Player: 3 } },
      });
      expect(useUiStore.getState().focusedOpponent).toBe(3);
    });

    it("click on a non-targetable opponent always focuses, never targets", async () => {
      // Only player 2 is a legal target. Clicking Opp 4 (player 3) — even
      // when already focused — must focus, never dispatch.
      const { dispatch } = mountWithTargeting([2]);
      useUiStore.setState({ focusedOpponent: 3 });
      render(<OpponentHud />);

      fireEvent.click(screen.getByRole("button", { name: /Opp 4/ }));

      await waitFor(() => {
        expect(useUiStore.getState().focusedOpponent).toBe(3);
      });
      expect(dispatch).not.toHaveBeenCalled();
    });

    it("tab tooltip reflects the next-click action (focus vs commit)", () => {
      mountWithTargeting();
      // Player 1 (Opp 2) starts focused, player 3 (Opp 4) does not.
      render(<OpponentHud />);

      // Unfocused + targetable → tooltip explains the two-step path.
      const unfocusedTitle = screen.getByRole("button", { name: /Opp 4/ }).getAttribute("title");
      expect(unfocusedTitle).toContain("click again to target");

      // Focused + targetable → tooltip is the commit verb only.
      expect(screen.getByRole("button", { name: "Target Opp 2" }))
        .toHaveAttribute("title", "Click to target Opp 2");
    });
  });

  it("renders compact poison and speed badges for the 1v1 opponent HUD", () => {
    const gameState = createGameState({
      players: [
        { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
        { id: 1, life: 20, poison_counters: 4, speed: 1, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      ],
      active_player: 1,
      priority_player: 1,
      waiting_for: { type: "Priority", data: { player: 1 } },
      seat_order: [0, 1],
      format_config: {
        format: "Standard",
        starting_life: 20,
        min_players: 2,
        max_players: 2,
        deck_size: 60,
        singleton: false,
        command_zone: false,
        commander_damage_threshold: null,
        range_of_influence: null,
        team_based: false,
        uses_commander: false,

        allow_debug_actions: false,
      },
    });

    act(() => {
      useGameStore.setState({ gameState });
    });

    render(<OpponentHud />);

    expect(screen.getByTitle("Poison counters: 4")).toHaveAttribute("aria-label", "4 poison counters");
    expect(screen.getByTitle("Speed: 1")).toHaveAttribute("aria-label", "Speed 1");
    expect(screen.queryByText("Speed")).toBeNull();
  });

  it("opens player enchantments dialog when the opponent aura badge is tapped", async () => {
    const gameState = createGameState({
      derived: {
        auras_attached_to_player: { "1": [101] },
      },
    });
    act(() => {
      useGameStore.setState({ gameState });
      useUiStore.setState({ enchantmentsDialogPlayer: null, focusedOpponent: 1 });
    });

    render(<OpponentHud />);

    fireEvent.click(screen.getByTestId("opponent-aura-badge-1"));

    await waitFor(() => {
      expect(useUiStore.getState().enchantmentsDialogPlayer).toBe(1);
    });
  });

  it("keeps compact-mode aura badge inside the tab for mobile-reachable hit area", () => {
    const gameState = createGameState({
      derived: {
        auras_attached_to_player: { "1": [101] },
      },
    });
    act(() => {
      usePreferencesStore.setState({ opponentHudDensity: "compact" });
      useGameStore.setState({ gameState });
      useUiStore.setState({ focusedOpponent: 1 });
    });

    render(<OpponentHud />);

    const badge = screen.getByTestId("opponent-aura-badge-1");
    expect(badge.className).toContain("-top-1.5");
    expect(badge.className).not.toContain("-bottom-5");
  });

  it("does not open the desktop aura hover preview on mobile", () => {
    setViewportWidth(500);
    const gameState = createGameState({
      objects: {
        "101": buildGameObject({
          id: 101,
          name: "Curse of Test",
          controller: 1,
          owner: 1,
        }),
      },
      derived: {
        auras_attached_to_player: { "1": [101] },
      },
    });
    act(() => {
      useGameStore.setState({ gameState });
      useUiStore.setState({ focusedOpponent: 1 });
    });

    render(<OpponentHud />);

    fireEvent.mouseEnter(screen.getByTestId("opponent-aura-badge-1"));

    expect(screen.queryByLabelText(/Curse of Test/i)).toBeNull();
  });
});
