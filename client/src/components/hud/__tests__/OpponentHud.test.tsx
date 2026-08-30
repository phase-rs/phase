import { act } from "react";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
  gameObjectFactory,
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
  targetSelectionWaitingForFactory,
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
    // `useCanActForWaitingState` short-circuits on EITHER `gameMode === "spectate"`
    // OR `isSpectator`, and the two live in different module-singleton stores that
    // persist across tests in this file. Both are reset here, not only in
    // `afterEach`, so one spectator row cannot make every later seated row inert.
    useMultiplayerStore.setState({
      activePlayerId: 0,
      isSpectator: false,
      playerAvatars: new Map(),
    });
    usePreferencesStore.setState({ followActiveOpponent: false, battlefieldPeekOnHover: false });
    useUiStore.setState({ focusedOpponent: 1 });
    useGameStore.setState({ gameState: createGameState(), gameMode: null, waitingFor: null });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders Next Up badge on the next multiplayer opponent tab", () => {
    useGameStore.setState({
      gameState: createGameState({
        derived: {
          turn_order: [{ player: 2, slot_index: 1, turns_from_now: 1, turn_number: 2 }],
        },
      }),
    });

    render(<OpponentHud />);

    expect(screen.getByTitle("This player's turn is next.")).toHaveTextContent("Next Up");
  });

  it("renders opponent tabs clockwise from a non-zero viewer, including eliminated seats", () => {
    useMultiplayerStore.setState({ activePlayerId: 1 });
    useUiStore.setState({ focusedOpponent: 2 });
    useGameStore.setState({
      gameMode: "online",
      gameState: createGameState({
        seat_order: [0, 3, 1, 2],
        eliminated_players: [0],
        derived: {
          turn_order: [
            { player: 2, slot_index: 1, turns_from_now: 1, turn_number: 2 },
            { player: 3, slot_index: 2, turns_from_now: 2, turn_number: 3 },
            { player: 1, slot_index: 3, turns_from_now: 3, turn_number: 4 },
          ],
        },
      }),
    });

    render(<OpponentHud />);

    const tabs = Array.from(document.querySelectorAll('button[data-player-hud]'));
    expect(tabs.map((tab) => tab.getAttribute("data-player-hud"))).toEqual(["2", "0", "3"]);
    expect(tabs[1]).toBeDisabled();
    expect(screen.getByTitle("This player's turn is next.")).toHaveTextContent("Next Up");
  });

  it("shows a tooltip and hover preview for opponent avatars with art", async () => {
    useMultiplayerStore.setState({
      playerAvatars: new Map([
        [1, { kind: "external", url: "https://example.test/opponent-avatar.jpg" }],
      ]),
    });

    render(<OpponentHud />);

    const avatar = screen.getByTitle("Opp 2");
    expect(avatar).toBeInTheDocument();

    fireEvent.mouseEnter(avatar);

    await waitFor(() => {
      expect(screen.getAllByAltText("Opp 2")).toHaveLength(2);
    });

    const [primary] = screen.getAllByAltText("Opp 2");
    fireEvent.error(primary);
    expect(screen.queryByRole("img", { name: "Opp 2" })).not.toBeInTheDocument();
    expect(screen.getByTitle("Opp 2")).toHaveTextContent("O");
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

  // ── The player axis reads one authority behind one actor gate ─────────────
  //
  // `OpponentHud` now derives `clickTargetRefs` (the raw `TargetRef[] | null`)
  // and `validPlayerTargetIds` (its player projection) from
  // `getWaitingForClickTargetRefs` / `getWaitingForPlayerChoiceIds`, both gated
  // on `useCanActForWaitingState()`.

  /** A `TargetSelection` addressed to the local seat with the given legal set. */
  function targetingLocalSeat(legal: TargetRef[]): WaitingFor {
    return targetSelectionWaitingForFactory
      .withData({
        selection: buildTargetSelectionProgress({ current_legal_targets: legal }),
        target_slots: [buildTargetSelectionSlot({ legal_targets: legal })],
        pending_cast: buildPendingCast(),
      })
      .forPlayer(0)
      .build();
  }

  // V8 row 2 — the multiplayer tab. `OpponentHud` exposes the `Target <name>`
  // accessible name only on an ALREADY-FOCUSED tab (the two-click FFA model), so
  // seat 1 is pre-focused in `beforeEach`; without that the assertion would pass
  // with or without the fix. The four seated FFA-disambiguation tests above are
  // this row's reach guards.
  it("offers no target commit on a multiplayer tab to a spectating client", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const wf = targetingLocalSeat([{ Player: 1 }]);
    act(() => {
      useMultiplayerStore.setState({ isSpectator: true });
      useUiStore.setState({ focusedOpponent: 1 });
      useGameStore.setState({
        dispatch,
        gameMode: "spectate",
        gameState: createGameState({ waiting_for: wf }),
        waitingFor: wf,
      });
    });

    render(<OpponentHud />);

    expect(screen.queryByRole("button", { name: "Target Opp 2" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /Opp 2/ }));
    expect(dispatch).not.toHaveBeenCalled();
  });

  // V8 row 3 — the 1v1 pill, the fourth seat surface. `HudPlate` renders a
  // `<button>` when it has an `onClick` and a `<div>` otherwise, so
  // `[data-hud-plate]`'s tagName reads `isValidTarget` directly. Two live seats
  // means `isMultiplayer` is false and exactly one plate renders.
  describe("1v1 opponent pill", () => {
    function mountOneOnOne(waitingFor: WaitingFor) {
      const dispatch = vi.fn().mockResolvedValue([]);
      act(() => {
        useGameStore.setState({
          dispatch,
          gameState: createGameState({
            players: buildPlayers([
              { id: 0, life: 20 },
              { id: 1, life: 20 },
            ]),
            seat_order: [0, 1],
            format_config: buildFormatConfig(),
            waiting_for: waitingFor,
          }),
          waitingFor,
        });
      });
      render(<OpponentHud />);
      return { dispatch };
    }

    it("offers the opponent pill to the seated player", () => {
      const { dispatch } = mountOneOnOne(targetingLocalSeat([{ Player: 1 }]));

      const plates = document.querySelectorAll("[data-hud-plate]");
      expect(plates).toHaveLength(1);
      expect(plates[0].tagName).toBe("BUTTON");

      fireEvent.click(plates[0]);
      expect(dispatch).toHaveBeenCalledWith({
        type: "ChooseTarget",
        data: { target: { Player: 1 } },
      });
    });

    it("offers no pill affordance to a spectating client", () => {
      act(() => {
        useMultiplayerStore.setState({ isSpectator: true });
        useGameStore.setState({ gameMode: "spectate" });
      });
      const { dispatch } = mountOneOnOne(targetingLocalSeat([{ Player: 1 }]));

      const plates = document.querySelectorAll("[data-hud-plate]");
      expect(plates).toHaveLength(1);
      expect(plates[0].tagName).toBe("DIV");

      fireEvent.click(plates[0]);
      expect(dispatch).not.toHaveBeenCalled();
    });
  });

  // V9 / V10 / V13 — the two OTHER consumers of the rewritten derivation, both
  // inside `OpponentTab`: `legalObjectTargetsByController` (which now iterates
  // `clickTargetRefs ?? []`) and `isTargeting` (now `clickTargetRefs !== null`).
  //
  // MANDATORY `await`: `PortaledPopover` holds `pos === null` until a
  // `requestAnimationFrame` callback fires and returns `null` until then, so on
  // the tick `fireEvent.mouseEnter` returns NEITHER popover is in the DOM. A
  // synchronous read is a uniform false that makes the positives fail and the
  // negatives pass vacuously. Every assertion below — negatives included — comes
  // after an `await screen.findByText(...)` for the popover the row expects.
  describe("opponent tab hover popovers", () => {
    // Seat 1 is focused by `beforeEach`, so seat 2 is the non-focused tab these
    // rows hover. Distinct names keep the three cards individually addressable.
    const ALPHA = 401;
    const BETA = 402;
    const GAMMA = 403;

    function boardOfSeatTwo() {
      return {
        battlefield: [ALPHA, BETA, GAMMA],
        objects: buildObjectMap(
          gameObjectFactory.creature(1, 1).onBattlefield().ownedBy(2)
            .withId(ALPHA).named("Alpha Bear").build(),
          gameObjectFactory.creature(2, 2).onBattlefield().ownedBy(2)
            .withId(BETA).named("Beta Bear").build(),
          gameObjectFactory.creature(3, 3).onBattlefield().ownedBy(2)
            .withId(GAMMA).named("Gamma Bear").build(),
        ),
      };
    }

    function mountBoard(waitingFor: WaitingFor, overrides: Partial<GameState> = {}) {
      act(() => {
        usePreferencesStore.setState({ battlefieldPeekOnHover: true });
        useGameStore.setState({
          gameState: createGameState({
            ...boardOfSeatTwo(),
            waiting_for: waitingFor,
            ...overrides,
          }),
          waitingFor,
        });
      });
      render(<OpponentHud />);
    }

    const hoverSeatTwoTab = () =>
      fireEvent.mouseEnter(screen.getByRole("button", { name: /Opp 3/ }));

    /**
     * The peeked cards in render order, read through the P/T tile the popover
     * itself renders from `objects[group.ids[0]]`.
     *
     * Deliberately NOT read through `CardImage`'s "Loading {{name}}" aria-label:
     * `useCardImage` resolves asynchronously and caches by name across tests in
     * the same file, so which `CardImage` branch is mounted (loading placeholder
     * vs `<img alt>` vs artless text tile) depends on test ORDER. [measured] the
     * label-based read passed in isolation and failed third-in-file. The P/T
     * tile is owned by the popover and is unaffected by art resolution, so the
     * three Bears carry distinct P/T (Alpha 1/1, Beta 2/2, Gamma 3/3) purely to
     * make group order legible here.
     */
    const peekedPT = (popover: HTMLElement) =>
      within(popover).getAllByText(/^\d+\/\d+$/).map((el) => el.textContent);

    const openPeek = async () => {
      hoverSeatTwoTab();
      // MANDATORY await: PortaledPopover returns null until a rAF sets position.
      const heading = await screen.findByText("Opp 3's board");
      return heading.parentElement as HTMLElement;
    };

    const ALPHA_PT = "1/1";
    const BETA_PT = "2/2";
    const GAMMA_PT = "3/3";

    // V9 — `legalObjectTargetsByController` still reaches the popover and still
    // reorders it. This is the only reachable observable that reads
    // `legalTargetIds`: the " (not targetable)" legend needs > 12 permanents.
    it("sorts a legal object target to the front of the peek popover", async () => {
      mountBoard(targetingLocalSeat([{ Object: GAMMA }]));

      expect(peekedPT(await openPeek())).toEqual([GAMMA_PT, ALPHA_PT, BETA_PT]);
    });

    // Second positive: a different legal id gives a different order, so the sort
    // tracks the specific id rather than applying a fixed permutation.
    it("sorts whichever object the engine made legal to the front", async () => {
      mountBoard(targetingLocalSeat([{ Object: BETA }]));

      expect(peekedPT(await openPeek())).toEqual([BETA_PT, ALPHA_PT, GAMMA_PT]);
    });

    // Reach guard: with no targeting in progress the popover keeps battlefield
    // order, proving the reordering above is caused by the targeting state and
    // not by the fixture.
    it("keeps battlefield order when no targeting is in progress", async () => {
      mountBoard(buildPriorityWaitingFor({ data: { player: 2 } }));

      expect(peekedPT(await openPeek())).toEqual([ALPHA_PT, BETA_PT, GAMMA_PT]);
    });

    // ── V10 / V13: `isTargeting` picks which popover opens ──────────────────
    // `showIncomingOnHover` is `hasIncoming && !isFocused && !isTargeting`, so
    // with an attacker on the hovered tab it is `isTargeting` alone that decides.
    function withIncomingAttacker(): Partial<GameState> {
      return {
        combat: {
          attackers: [
            { object_id: ALPHA, defending_player: 0, attack_target: { type: "Player", data: 0 } },
          ],
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
      };
    }

    // V10 — the row that justifies the `TargetRef[] | null` return type. The
    // legal set is EMPTY, so a `(clickTargetRefs ?? []).length > 0` reading of
    // `isTargeting` would open the incoming popover instead.
    it("treats a live prompt with an empty legal set as targeting", async () => {
      mountBoard(targetingLocalSeat([]), withIncomingAttacker());

      hoverSeatTwoTab();
      await screen.findByText("Opp 3's board");

      expect(screen.queryAllByText(/incoming from/)).toHaveLength(0);
    });

    // Reach guard for V10: the same fixture with no prompt opens the incoming
    // popover, so the row above is about the gate rather than about a fixture
    // that renders nothing. Queried by `/incoming from/`, not `⚔×`: the
    // always-present attacker badge carries the same `⚔×` prefix.
    it("opens the incoming-attackers popover when no prompt is live", async () => {
      mountBoard(buildPriorityWaitingFor({ data: { player: 2 } }), withIncomingAttacker());

      hoverSeatTwoTab();
      await screen.findByText(/incoming from/);

      expect(screen.queryByText("Opp 3's board")).toBeNull();
    });

    // V13 — the spectator-only relaxation `showIncomingOnHover` inherits from the
    // new gate. A spectator is not targeting, so the threat-surfacing overlay
    // should no longer be suppressed on their screen. Row (a) below is the
    // seated reach guard, proving the fixture really does produce `hasIncoming`
    // on a hoverable non-focused tab.
    it("suppresses the incoming popover for the seated player who is targeting", async () => {
      mountBoard(targetingLocalSeat([{ Player: 2 }]), withIncomingAttacker());

      hoverSeatTwoTab();
      await screen.findByText("Opp 3's board");

      expect(screen.queryAllByText(/incoming from/)).toHaveLength(0);
    });

    it("shows the incoming popover to a spectator during the same prompt", async () => {
      act(() => {
        useMultiplayerStore.setState({ isSpectator: true });
        useGameStore.setState({ gameMode: "spectate" });
      });
      mountBoard(targetingLocalSeat([{ Player: 2 }]), withIncomingAttacker());

      hoverSeatTwoTab();
      await screen.findByText(/incoming from/);

      expect(screen.queryByText("Opp 3's board")).toBeNull();
    });
  });
});
