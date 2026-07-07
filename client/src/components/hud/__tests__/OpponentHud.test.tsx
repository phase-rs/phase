import { act } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, TargetRef, WaitingFor } from "../../../adapter/types.ts";
import { OpponentHud } from "../OpponentHud.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import {
  buildGameObject,
  buildObjectMap,
} from "../../../test/factories/gameObjectFactory.ts";
import {
  buildCommanderFormatConfig,
  buildFormatConfig,
  buildGameState,
  buildPendingCast,
  buildPlayers,
  buildPriorityWaitingFor,
  buildTargetSelectionProgress,
  buildTargetSelectionSlot,
  buildTargetSelectionWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";

function setViewportWidth(width: number) {
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    writable: true,
    value: width,
  });
  window.dispatchEvent(new Event("resize"));
}

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return buildGameState({
    active_player: 2,
    players: buildPlayers([
      { id: 0, life: 40 },
      { id: 1, life: 40 },
      { id: 2, life: 40 },
      { id: 3, life: 40 },
    ]),
    priority_player: 2,
    waiting_for: buildPriorityWaitingFor({ data: { player: 2 } }),
    seat_order: [0, 1, 2, 3],
    format_config: buildCommanderFormatConfig(),
    ...overrides,
  });
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

  it("renders Next Up badge on the next multiplayer opponent tab", () => {
    useGameStore.setState({
      gameState: createGameState({
        derived: {
          turn_order: [{ player: 2, slot_index: 1, turns_from_now: 1 }],
        },
      }),
    });

    render(<OpponentHud />);

    expect(screen.getByTitle("This player's turn is next.")).toHaveTextContent("Next Up");
  });

  it("keeps multiplayer opponent tabs in stable seat order while marking next up", () => {
    useGameStore.setState({
      gameState: createGameState({
        seat_order: [0, 3, 1, 2],
        derived: {
          turn_order: [
            { player: 2, slot_index: 1, turns_from_now: 1 },
            { player: 3, slot_index: 2, turns_from_now: 2 },
            { player: 1, slot_index: 3, turns_from_now: 3 },
          ],
        },
      }),
    });

    render(<OpponentHud />);

    const tabs = Array.from(document.querySelectorAll('button[data-player-hud]'));
    expect(tabs.map((tab) => tab.getAttribute("data-player-hud"))).toEqual(["3", "1", "2"]);
    expect(screen.getByTitle("This player's turn is next.")).toHaveTextContent("Next Up");
  });

  it("shows a tooltip and hover preview for opponent avatars with art", async () => {
    useMultiplayerStore.setState({
      playerAvatars: new Map([[1, "https://example.test/opponent-avatar.jpg"]]),
    });

    render(<OpponentHud />);

    const avatar = screen.getByTitle("Opp 2");
    expect(avatar).toBeInTheDocument();

    fireEvent.mouseEnter(avatar);

    await waitFor(() => {
      expect(screen.getAllByAltText("Opp 2")).toHaveLength(2);
    });
  });

  it("auto-selects the active opponent when Follow is enabled", async () => {
    render(<OpponentHud />);

    fireEvent.click(screen.getByRole("button", { name: /follow active opponent/i }));

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(2);
    });

    act(() => {
      useGameStore.setState({
        gameState: createGameState({
          active_player: 3,
          priority_player: 3,
          waiting_for: buildPriorityWaitingFor({ data: { player: 3 } }),
        }),
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
        gameState: createGameState({
          active_player: 1,
          priority_player: 1,
          waiting_for: buildPriorityWaitingFor({ data: { player: 1 } }),
        }),
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

  it("refocuses onto a live opponent when the focused seat is eliminated", async () => {
    useUiStore.setState({ focusedOpponent: 1 });
    const { rerender } = render(<OpponentHud />);

    act(() => {
      useGameStore.setState({
        gameState: createGameState({
          eliminated_players: [1, 2],
        }),
      });
    });
    rerender(<OpponentHud />);

    await waitFor(() => {
      expect(useUiStore.getState().focusedOpponent).toBe(3);
    });
  });

  it("expands the comfortable HUD after toggling out of compact mode", () => {
    usePreferencesStore.setState({ opponentHudDensity: "compact" });
    useUiStore.setState({ focusedOpponent: 3 });
    render(<OpponentHud />);

    fireEvent.click(screen.getByRole("button", { name: /expand opponent hud/i }));

    expect(usePreferencesStore.getState().opponentHudDensity).toBe("comfortable");
  });

  it("forces compact opponent tabs in split overview without changing the saved density", () => {
    usePreferencesStore.setState({ opponentHudDensity: "comfortable" });

    render(<OpponentHud splitOverview />);

    expect(screen.queryByRole("button", { name: /compact opponent hud/i })).toBeNull();
    expect(screen.queryByText(/hand/i)).toBeNull();
    expect(screen.queryByText(/creatures/i)).toBeNull();
    expect(screen.queryByText(/lands/i)).toBeNull();
    expect(usePreferencesStore.getState().opponentHudDensity).toBe("comfortable");
  });

  it("keeps Follow enabled when browsing opponents on my turn", async () => {
    usePreferencesStore.setState({ followActiveOpponent: true });
    useGameStore.setState({
      gameState: createGameState({
        active_player: 0,
        priority_player: 0,
        waiting_for: buildPriorityWaitingFor({ data: { player: 0 } }),
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
        gameState: createGameState({
          active_player: 2,
          priority_player: 2,
          waiting_for: buildPriorityWaitingFor({ data: { player: 2 } }),
        }),
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

    // Custom GameplayTooltip text in the DOM replaces the native `title`.
    expect(screen.getByLabelText("3 poison counters")).toBeInTheDocument();
    expect(screen.getByText("Poison counters: 3")).toBeInTheDocument();
    expect(screen.getByLabelText("Speed 2")).toBeInTheDocument();
    expect(screen.getByText("Speed: 2")).toBeInTheDocument();
    expect(screen.queryByText("Speed")).toBeNull();
  });

  it("hides zero poison counters", () => {
    render(<OpponentHud />);

    expect(screen.queryByText(/Poison counters:/)).toBeNull();
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
      return buildTargetSelectionWaitingFor({
        data: {
          player: 0,
          selection: buildTargetSelectionProgress({ current_legal_targets: targets }),
          target_slots: [buildTargetSelectionSlot({ legal_targets: targets })],
          pending_cast: buildPendingCast(),
        },
      });
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
      players: buildPlayers([
        { id: 0, life: 20 },
        { id: 1, life: 20, poison_counters: 4, speed: 1 },
      ]),
      active_player: 1,
      priority_player: 1,
      waiting_for: buildPriorityWaitingFor({ data: { player: 1 } }),
      seat_order: [0, 1],
      format_config: buildFormatConfig(),
    });

    act(() => {
      useGameStore.setState({ gameState });
    });

    render(<OpponentHud />);

    expect(screen.getByLabelText("4 poison counters")).toBeInTheDocument();
    expect(screen.getByText("Poison counters: 4")).toBeInTheDocument();
    expect(screen.getByLabelText("Speed 1")).toBeInTheDocument();
    expect(screen.getByText("Speed: 1")).toBeInTheDocument();
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
    expect(badge.className).toContain("-bottom-1.5");
    expect(badge.className).not.toContain("-bottom-5");
  });

  it("does not open the desktop aura hover preview on mobile", () => {
    setViewportWidth(500);
    const gameState = createGameState({
      objects: buildObjectMap(
        buildGameObject({
          id: 101,
          name: "Curse of Test",
          controller: 1,
          owner: 1,
        }),
      ),
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

  it("uses the single opponent pill when a 4-player pod has one live rival (#1324)", () => {
    act(() => {
      useGameStore.setState({
        gameState: createGameState({
          eliminated_players: [1, 2],
          active_player: 3,
          priority_player: 3,
          waiting_for: buildPriorityWaitingFor({ data: { player: 3 } }),
        }),
      });
      useUiStore.setState({ focusedOpponent: 1 });
    });

    render(<OpponentHud />);

    expect(document.querySelector('[data-player-hud="3"]')).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Opp 2/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /OUT/i })).toBeNull();
  });
});
