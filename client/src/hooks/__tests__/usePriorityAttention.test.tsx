import { act, cleanup, renderHook } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, PlayerId, WaitingFor } from "../../adapter/types.ts";
import {
  ESCALATE_AFTER_MS,
  MIN_WAIT_BEFORE_ALERT_MS,
} from "../../game/priorityAttention.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { usePriorityAttentionStore } from "../../stores/priorityAttentionStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { buildGameObject } from "../../test/factories/gameObjectFactory.ts";
import {
  buildFormatConfig,
  buildGameState,
  buildManaPaymentWaitingFor,
  buildPriorityWaitingFor,
  buildStackEntry,
} from "../../test/factories/gameStateFactory.ts";
import {
  usePriorityAttention,
  usePriorityAttentionClass,
  usePriorityAttentionController,
} from "../usePriorityAttention.ts";

const LOCAL: PlayerId = 0;
const OPPONENT: PlayerId = 1;

// The page reads as in front of the player unless a test says otherwise, rather
// than relying on the DOM implementation's default for `hasFocus()`.
beforeEach(() => {
  vi.spyOn(document, "hasFocus").mockReturnValue(true);
});
afterEach(() => {
  vi.restoreAllMocks();
});

/** `shouldAutoPass` treats an object-less state as "not loaded yet" and never
 *  auto-passes it, so every fixture carries one object. */
function stateWithPriorityOn(player: PlayerId, activePlayer: PlayerId = OPPONENT): GameState {
  return buildGameState({
    priority_player: player,
    active_player: activePlayer,
    objects: { "1": buildGameObject({ id: 1 }) },
  });
}

/** Commit a fresh engine snapshot the way the game store does: gameState,
 *  waitingFor and autoPassRecommended land together. */
function commit(
  player: PlayerId,
  autoPassRecommended: boolean,
  activePlayer: PlayerId = OPPONENT,
): WaitingFor {
  const waitingFor = buildPriorityWaitingFor({
    type: "Priority",
    data: { player },
  });
  act(() => {
    useGameStore.setState({
      gameState: stateWithPriorityOn(player, activePlayer),
      waitingFor,
      autoPassRecommended,
    });
  });
  return waitingFor;
}

function mount() {
  return renderHook(() => {
    usePriorityAttentionController();
    return usePriorityAttention();
  });
}

describe("usePriorityAttentionController", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useGameStore.setState({
      gameMode: null,
      gameState: null,
      waitingFor: null,
      autoPassRecommended: false,
    });
    useUiStore.setState({ fullControl: false });
    useMultiplayerStore.setState({ isSpectator: false, activePlayerId: null });
    usePriorityAttentionStore.setState({ cue: "none", chimeCount: 0 });
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    useGameStore.setState({ gameState: null, waitingFor: null, autoPassRecommended: false });
    usePriorityAttentionStore.setState({ cue: "none", chimeCount: 0 });
  });

  it("announces a meaningful local priority window after a long idle wait, with the glow and no chime", () => {
    const { result } = mount();

    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false);

    expect(result.current).toEqual({ cue: "announced", chimeCount: 0 });
  });

  it("stays silent when the wait was shorter than the minimum", () => {
    const { result } = mount();

    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS - 1);
    });
    commit(LOCAL, false);

    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });
  });

  it("stays silent when the engine recommends auto-passing the window", () => {
    const { result } = mount();

    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS * 2);
    });
    commit(LOCAL, true);

    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });
  });

  it("announces an auto-pass recommended window under Full Control", () => {
    const { result } = mount();

    act(() => {
      useUiStore.setState({ fullControl: true });
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, true);

    expect(result.current.cue).toBe("announced");
    expect(result.current.chimeCount).toBe(0);
  });

  it("stays silent when the opponent holds priority", () => {
    const { result } = mount();

    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS * 2);
    });
    commit(OPPONENT, false);

    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });
  });

  it("escalates after ESCALATE_AFTER_MS and clears when the window closes", () => {
    const { result } = mount();

    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false);
    expect(result.current.cue).toBe("announced");
    expect(result.current.cue).not.toBe("reminded");

    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS - 1);
    });
    expect(result.current.cue).not.toBe("reminded");

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current).toEqual({ cue: "reminded", chimeCount: 1 });

    // The player passes; priority moves to the opponent and the window closes.
    commit(OPPONENT, false);
    expect(result.current).toEqual({ cue: "none", chimeCount: 1 });
  });

  it("keeps the announcement through re-commits of the same decision", () => {
    const { result } = mount();

    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false);
    expect(result.current.cue).toBe("announced");

    // The store replaces `waitingFor` on every accepted snapshot, including an
    // equal-sequence re-read of the same engine state: no clear, no re-fire.
    commit(LOCAL, false);
    commit(LOCAL, false);
    expect(result.current).toEqual({ cue: "announced", chimeCount: 0 });
  });

  it.each([
    ["pointerdown", () => new Event("pointerdown", { bubbles: true })],
    ["keydown", () => new KeyboardEvent("keydown", { key: "a", bubbles: true })],
  ])("clears the announcement on a real %s and stays silent", (_name, makeEvent) => {
    const { result } = mount();

    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false);
    expect(result.current.cue).toBe("announced");

    act(() => {
      document.body.dispatchEvent(makeEvent());
    });
    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });

    // Still holding priority after acting (e.g. played a land): stays quiet.
    commit(LOCAL, false);
    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });

    // A later window after a long wait announces again.
    commit(OPPONENT, false);
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false);
    expect(result.current).toEqual({ cue: "announced", chimeCount: 0 });
  });

  it("does not escalate once the player has answered", () => {
    const { result } = mount();

    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false);
    act(() => {
      document.body.dispatchEvent(new Event("pointerdown", { bubbles: true }));
      vi.advanceTimersByTime(ESCALATE_AFTER_MS * 2);
    });

    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });
  });

  it("clears a pending announcement on unmount", () => {
    const { result, unmount } = mount();

    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false);
    expect(result.current.cue).toBe("announced");

    unmount();

    expect(usePriorityAttentionStore.getState()).toMatchObject({ cue: "none", chimeCount: 0 });
  });

  it("is silent for a controlled player: the window's authorized submitter is their controller", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS * 2);
    });

    // CR 723.5: seat 0 holds the semantic priority but seat 1 controls the turn
    // and is the authorized submitter, for a pass-only window and a real one.
    for (const autoPassRecommended of [true, false]) {
      act(() => {
        useGameStore.setState({
          gameState: buildGameState({
            priority_player: OPPONENT,
            active_player: LOCAL,
            turn_decision_controller: OPPONENT,
            objects: { "1": buildGameObject({ id: 1 }) },
          }),
          waitingFor: buildPriorityWaitingFor({ type: "Priority", data: { player: LOCAL } }),
          autoPassRecommended,
        });
      });
      expect(result.current).toEqual({ cue: "none", chimeCount: 0 });
    }
  });

  it("does not count time spent on their own decision as waiting", () => {
    const { result } = mount();

    // A long local mana payment, then priority straight after it.
    act(() => {
      useGameStore.setState({
        gameState: stateWithPriorityOn(LOCAL),
        waitingFor: buildManaPaymentWaitingFor({ type: "ManaPayment", data: { player: LOCAL } }),
        autoPassRecommended: false,
      });
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS * 3);
    });
    commit(LOCAL, false);
    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });

    // A genuine opponent wait afterwards still announces.
    commit(OPPONENT, false);
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false);
    expect(result.current).toEqual({ cue: "announced", chimeCount: 0 });
  });

  it("resets on a new game session: nothing pending, no idle time carried over", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false);
    expect(result.current.cue).toBe("announced");

    // A replaced game: a fresh local Priority snapshot under a new generation.
    act(() => {
      useGameStore.setState({
        gameSessionGeneration: useGameStore.getState().gameSessionGeneration + 1,
        gameState: stateWithPriorityOn(LOCAL),
        waitingFor: buildPriorityWaitingFor({ type: "Priority", data: { player: LOCAL } }),
        autoPassRecommended: false,
      });
    });
    // Nothing pending and no idle time carried over.
    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });
  });

  it("never escalates an own-turn window the player may simply be thinking about", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false, LOCAL); // my turn, empty stack: the open main-phase window
    expect(result.current).toEqual({ cue: "announced", chimeCount: 0 });

    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS * 10);
    });
    expect(result.current).toEqual({ cue: "announced", chimeCount: 0 });
  });

  it("never escalates a planning window even with something on the stack", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    act(() => {
      useGameStore.setState({
        gameState: buildGameState({
          priority_player: LOCAL,
          active_player: LOCAL,
          stack: [buildStackEntry({ id: 50 })],
          objects: { "1": buildGameObject({ id: 1 }) },
        }),
        waitingFor: buildPriorityWaitingFor({ type: "Priority", data: { player: LOCAL } }),
        autoPassRecommended: false,
      });
    });
    expect(result.current.cue).toBe("announced");

    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS * 10);
    });
    expect(result.current).toEqual({ cue: "announced", chimeCount: 0 });
  });

  it("treats planning on behalf of a controlled active player as planning, not reactive", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    // Seat 0 controls seat 2's turn: seat 2 is active and holds semantic
    // priority, seat 0 is the authorized submitter.
    act(() => {
      useGameStore.setState({
        gameState: buildGameState({
          priority_player: LOCAL,
          active_player: 2,
          turn_decision_controller: LOCAL,
          objects: { "1": buildGameObject({ id: 1 }) },
        }),
        waitingFor: buildPriorityWaitingFor({ type: "Priority", data: { player: 2 } }),
        autoPassRecommended: false,
      });
    });
    expect(result.current.cue).toBe("announced");

    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS * 4);
    });
    expect(result.current.cue).not.toBe("reminded");
  });

  it("escalates an unanswered reactive window at the 15 second boundary", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false); // the opponent's turn
    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS - 1);
    });
    expect(result.current.cue).not.toBe("reminded");
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current).toEqual({ cue: "reminded", chimeCount: 1 });
  });

  it("reminds a reactive window that opened too soon to announce, and chimes with the reminder", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    commit(LOCAL, false);
    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });

    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS);
    });
    expect(result.current).toEqual({ cue: "reminded", chimeCount: 1 });
  });

  it("does not remind a quiet window the player answered", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    commit(LOCAL, false);
    act(() => {
      document.body.dispatchEvent(new Event("pointerdown", { bubbles: true }));
      vi.advanceTimersByTime(ESCALATE_AFTER_MS * 2);
    });
    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });
  });

  it("keeps one reminder timer across re-commits of the same window", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false);
    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS - 2_000);
    });
    commit(LOCAL, false); // an equal-sequence re-read must not restart the deadline
    act(() => {
      vi.advanceTimersByTime(2_000);
    });
    expect(result.current.cue).toBe("reminded");
  });

  it("runs one reminder timer through a kind change, a re-commit, StrictMode and unmount", () => {
    const { result, unmount } = renderHook(
      () => {
        usePriorityAttentionController();
        return usePriorityAttention();
      },
      { wrapper: StrictMode },
    );

    // A quiet reactive window arms exactly one reminder.
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    commit(LOCAL, false);
    expect(vi.getTimerCount()).toBe(1);

    // The same window becomes planning before the deadline: the timer is cancelled.
    act(() => {
      vi.advanceTimersByTime(5_000);
    });
    commit(LOCAL, false, LOCAL);
    expect(vi.getTimerCount()).toBe(0);
    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS * 4);
    });
    expect(result.current.cue).not.toBe("reminded");

    // Reactive again: the deadline is re-anchored at the transition, not at the
    // original opening, and re-commits keep the single timer.
    commit(LOCAL, false);
    expect(vi.getTimerCount()).toBe(1);
    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS - 1);
    });
    commit(LOCAL, false);
    expect(result.current.cue).not.toBe("reminded");
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current.cue).toBe("reminded");
    expect(vi.getTimerCount()).toBe(0);

    // The reminder, and its chime, do not repeat, and unmounting leaves nothing running or showing.
    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS * 4);
    });
    expect(usePriorityAttentionStore.getState().chimeCount).toBe(1);
    unmount();
    expect(vi.getTimerCount()).toBe(0);
    expect(usePriorityAttentionStore.getState()).toMatchObject({ cue: "none" });
  });

  it("does not count time spent ordering triggers as waiting before the next priority window", () => {
    const { result } = mount();

    // A local trigger-ordering prompt is the player's own decision; the
    // priority window that follows it is not a handoff after being away.
    act(() => {
      useGameStore.setState({
        gameState: stateWithPriorityOn(LOCAL),
        waitingFor: { type: "OrderTriggers", data: { player: LOCAL, triggers: [] } },
        autoPassRecommended: false,
      });
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS * 3);
    });
    commit(LOCAL, false);
    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });
  });

  it("does not treat time spent on a mulligan as waiting before the first window", () => {
    const { result } = mount();

    // A simultaneous prompt carries `pending`, not a `player`, so it reads as
    // away; the player is plainly at the screen, and says so by clicking Keep.
    act(() => {
      useGameStore.setState({
        gameState: stateWithPriorityOn(LOCAL, LOCAL),
        waitingFor: {
          type: "MulliganDecision",
          data: {
            pending: [{ player: LOCAL, mulligan_count: 0, phase: { type: "Declare" } }],
            free_first_mulligan: false,
          },
        },
        autoPassRecommended: false,
      });
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS * 3);
    });
    act(() => {
      document.body.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    });
    commit(LOCAL, false, LOCAL); // the first main phase, on the play
    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });
  });

  it("gives a window that follows an answered one its own reminder", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false);
    expect(result.current.cue).toBe("announced");

    // The player passes; the engine auto-passes the other seats and hands
    // priority back at the next step with no away commit in between.
    act(() => {
      document.body.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    });
    expect(result.current.cue).toBe("none");
    act(() => {
      useGameStore.setState({
        gameState: buildGameState({
          priority_player: LOCAL,
          active_player: OPPONENT,
          phase: "PostCombatMain",
          objects: { "1": buildGameObject({ id: 1 }) },
        }),
        waitingFor: buildPriorityWaitingFor({ type: "Priority", data: { player: LOCAL } }),
        autoPassRecommended: false,
      });
    });
    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });

    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS);
    });
    expect(result.current).toEqual({ cue: "reminded", chimeCount: 1 });
  });

  it("keeps an unanswered announcement when the window changes under the player", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false);
    act(() => {
      useGameStore.setState({
        gameState: buildGameState({
          priority_player: LOCAL,
          active_player: OPPONENT,
          stack: [buildStackEntry({ id: 50 })],
          objects: { "1": buildGameObject({ id: 1 }) },
        }),
        waitingFor: buildPriorityWaitingFor({ type: "Priority", data: { player: LOCAL } }),
        autoPassRecommended: false,
      });
    });
    expect(result.current).toEqual({ cue: "announced", chimeCount: 0 });
  });
});

describe("windows that differ only below the phase", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useGameStore.setState({ gameMode: null, gameState: null, waitingFor: null, autoPassRecommended: false });
    useUiStore.setState({ fullControl: false });
    useMultiplayerStore.setState({ isSpectator: false, activePlayerId: null });
    usePriorityAttentionStore.setState({ cue: "none", chimeCount: 0 });
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    useGameStore.setState({ gameState: null, waitingFor: null, autoPassRecommended: false });
    usePriorityAttentionStore.setState({ cue: "none", chimeCount: 0 });
  });

  const answer = () =>
    act(() => {
      document.body.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    });

  const commitControlled = (semantic: PlayerId, stackIds: number[] = [70]) =>
    act(() => {
      useGameStore.setState({
        // Seat 0 controls seat 2's turn: it is the authorized submitter for both
        // seats, but the semantic holder changes between the two windows.
        gameState: buildGameState({
          turn_number: 5,
          phase: "PreCombatMain",
          priority_player: LOCAL,
          active_player: 2,
          turn_decision_controller: LOCAL,
          stack: stackIds.map((id) => buildStackEntry({ id })),
          objects: { "1": buildGameObject({ id: 1 }) },
        }),
        waitingFor: buildPriorityWaitingFor({ type: "Priority", data: { player: semantic } }),
        autoPassRecommended: false,
      });
    });

  it("gives seat 0's own window its reminder after it answered the controlled seat's window", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commitControlled(2); // acting for the active seat: planning
    expect(result.current.cue).toBe("announced");

    answer();
    commitControlled(LOCAL); // priority passes to seat 0 itself, same turn, step and stack
    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });

    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS);
    });
    expect(result.current).toEqual({ cue: "reminded", chimeCount: 1 });
  });

  it("gives the regular combat damage step its own reminder after the first-strike step was answered", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    const commitDamageStep = (occurrence: number) =>
      act(() => {
        useGameStore.setState({
          gameState: buildGameState({
            turn_number: 5,
            phase: "CombatDamage",
            steps_started_this_turn: { CombatDamage: occurrence },
            priority_player: LOCAL,
            active_player: OPPONENT,
            objects: { "1": buildGameObject({ id: 1 }) },
          }),
          waitingFor: buildPriorityWaitingFor({ type: "Priority", data: { player: LOCAL } }),
          autoPassRecommended: false,
        });
      });

    commitDamageStep(1); // first-strike damage on the opponent's turn
    expect(result.current.cue).toBe("announced");
    answer();

    // The opponent is yielding, so the engine hands priority straight back at
    // the regular damage step: same turn, phase, seat and (empty) stack.
    commitDamageStep(2);
    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });

    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS);
    });
    expect(result.current).toEqual({ cue: "reminded", chimeCount: 1 });
  });

  it("never reminds in a team format, where a teammate's window is the team's own planning window", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    // Two-Headed Giant: seat 1 (a teammate) is active; seat 0 holds the team's priority.
    act(() => {
      useGameStore.setState({
        gameState: buildGameState({
          priority_player: LOCAL,
          active_player: OPPONENT,
          format_config: buildFormatConfig({ team_based: true }),
          objects: { "1": buildGameObject({ id: 1 }) },
        }),
        waitingFor: buildPriorityWaitingFor({ type: "Priority", data: { player: LOCAL } }),
        autoPassRecommended: false,
      });
    });
    expect(result.current.cue).toBe("announced");

    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS * 4);
    });
    expect(result.current).toEqual({ cue: "announced", chimeCount: 0 });
  });

  it("gives a window whose stack changed at the same depth its own reminder", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commitControlled(LOCAL, [70]);
    expect(result.current.cue).toBe("announced");

    answer();
    commitControlled(LOCAL, [71]); // the top entry resolved and another took its place
    expect(result.current.cue).toBe("none");

    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS);
    });
    expect(result.current).toEqual({ cue: "reminded", chimeCount: 1 });
  });
});

describe("presence and visibility", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useGameStore.setState({ gameMode: null, gameState: null, waitingFor: null, autoPassRecommended: false });
    useUiStore.setState({ fullControl: false });
    useMultiplayerStore.setState({ isSpectator: false, activePlayerId: null });
    usePriorityAttentionStore.setState({ cue: "none", chimeCount: 0 });
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.useRealTimers();
    useGameStore.setState({ gameState: null, waitingFor: null, autoPassRecommended: false });
    usePriorityAttentionStore.setState({ cue: "none", chimeCount: 0 });
  });

  const press = (init: KeyboardEventInit) =>
    act(() => {
      document.body.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, ...init }));
    });

  it("announces and chimes a window that opens in a hidden tab sooner than the away gate", () => {
    vi.spyOn(document, "hidden", "get").mockReturnValue(true);
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    commit(LOCAL, false, LOCAL); // my own turn, planning: no reminder ever, but a hidden tab still hears it
    expect(result.current).toEqual({ cue: "announced", chimeCount: 1 });
  });

  it("announces and chimes a window held quiet when the tab is then hidden", () => {
    const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(false);
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    commit(LOCAL, false, LOCAL);
    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });

    hidden.mockReturnValue(true);
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(result.current).toEqual({ cue: "announced", chimeCount: 1 });
  });

  it("announces and chimes a window that replaces an answered one while the tab is hidden", () => {
    const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(false);
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false, LOCAL);
    expect(result.current).toEqual({ cue: "announced", chimeCount: 0 });

    // The player answers, then leaves the tab; priority comes straight back at the
    // next step with no away commit in between.
    act(() => {
      document.body.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    });
    hidden.mockReturnValue(true);
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(result.current).toEqual({ cue: "none", chimeCount: 0 }); // the answered window stays answered

    act(() => {
      useGameStore.setState({
        gameState: buildGameState({
          priority_player: LOCAL,
          active_player: LOCAL,
          phase: "PostCombatMain",
          objects: { "1": buildGameObject({ id: 1 }) },
        }),
        waitingFor: buildPriorityWaitingFor({ type: "Priority", data: { player: LOCAL } }),
        autoPassRecommended: false,
      });
    });
    expect(result.current).toEqual({ cue: "announced", chimeCount: 1 });
  });

  it("keeps counting upward across a restart, so an announcement in the restarting commit still chimes", () => {
    usePriorityAttentionStore.setState({ cue: "none", chimeCount: 3 }); // a previous game's count
    vi.spyOn(document, "hidden", "get").mockReturnValue(true);
    const { result } = mount();
    commit(LOCAL, false, LOCAL);
    expect(result.current).toEqual({ cue: "announced", chimeCount: 4 });
  });

  it("announces and chimes a window that opens while the window is unfocused, sooner than the away gate", () => {
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    commit(LOCAL, false, LOCAL);
    expect(result.current).toEqual({ cue: "announced", chimeCount: 1 });
  });

  it("chimes a window the player was looking at the moment the window loses focus", () => {
    const focus = vi.spyOn(document, "hasFocus").mockReturnValue(true);
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false, LOCAL);
    expect(result.current).toEqual({ cue: "announced", chimeCount: 0 });

    focus.mockReturnValue(false);
    act(() => {
      window.dispatchEvent(new Event("blur"));
    });
    expect(result.current).toEqual({ cue: "announced", chimeCount: 1 });

    // Leaving and returning never sounds the same window twice.
    act(() => {
      window.dispatchEvent(new Event("blur"));
      window.dispatchEvent(new Event("focus"));
    });
    focus.mockReturnValue(false);
    act(() => {
      window.dispatchEvent(new Event("blur"));
    });
    expect(result.current.chimeCount).toBe(1);
  });

  it("ignores a blur while the page still has focus (focus moved into an embedded frame)", () => {
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false, LOCAL);
    act(() => {
      window.dispatchEvent(new Event("blur"));
    });
    expect(result.current).toEqual({ cue: "announced", chimeCount: 0 });
  });

  it("does not chime an answered window when the player then leaves", () => {
    const focus = vi.spyOn(document, "hasFocus").mockReturnValue(true);
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false, LOCAL);
    act(() => {
      document.body.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    });
    focus.mockReturnValue(false);
    act(() => {
      window.dispatchEvent(new Event("blur"));
    });
    expect(result.current).toEqual({ cue: "none", chimeCount: 0 });
  });

  it("keeps the chime count and removes its listeners when the game screen unmounts", () => {
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    const removeFromWindow = vi.spyOn(window, "removeEventListener");
    const removeFromDocument = vi.spyOn(document, "removeEventListener");
    const { result, unmount } = mount();
    commit(LOCAL, false, LOCAL); // opens while the window is unfocused: sounds at once
    expect(result.current.chimeCount).toBe(1);

    unmount();
    expect(usePriorityAttentionStore.getState().chimeCount).toBe(1);
    expect(removeFromWindow).toHaveBeenCalledWith("blur", expect.any(Function));
    expect(removeFromDocument).toHaveBeenCalledWith("visibilitychange", expect.any(Function));
  });

  it("does not chime a window the player is looking at, however long it sits", () => {
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false, LOCAL); // own turn: planning, never reminded
    act(() => {
      vi.advanceTimersByTime(ESCALATE_AFTER_MS * 10);
    });
    expect(result.current).toEqual({ cue: "announced", chimeCount: 0 });
  });

  it.each([
    ["a bare Control", { key: "Control", ctrlKey: true }],
    ["a bare Alt", { key: "Alt", altKey: true }],
    ["a bare Meta", { key: "Meta", metaKey: true }],
    ["a bare Shift", { key: "Shift", shiftKey: true }],
    ["a bare AltGraph (sets no modifier flag on Linux or macOS)", { key: "AltGraph" }],
    ["Ctrl+Tab", { key: "Tab", ctrlKey: true }],
    ["Cmd+Tab", { key: "Tab", metaKey: true }],
    ["a held-down key", { key: "a", repeat: true }],
  ] as const)("does not take %s for the player being here", (_what, init) => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false);
    expect(result.current.cue).toBe("announced");

    press(init);
    expect(result.current.cue).toBe("announced");
  });

  it("still takes an ordinary key for the player being here", () => {
    const { result } = mount();
    act(() => {
      vi.advanceTimersByTime(MIN_WAIT_BEFORE_ALERT_MS);
    });
    commit(LOCAL, false);
    press({ key: " " });
    expect(result.current.cue).toBe("none");
  });
});

describe("usePriorityAttentionClass", () => {
  beforeEach(() => {
    usePreferencesStore.setState({ priorityAlertVisual: true });
    usePriorityAttentionStore.setState({ cue: "announced", chimeCount: 1 });
  });
  afterEach(() => {
    cleanup();
    usePreferencesStore.setState({ priorityAlertVisual: true });
    usePriorityAttentionStore.setState({ cue: "none", chimeCount: 0 });
  });

  it("is the base glow for an announced window the viewer can act on", () => {
    const { result } = renderHook(() => usePriorityAttentionClass(true));
    expect(result.current).toBe("priority-attention-glow");
  });

  it("adds the stronger glow once the reminder has fired", () => {
    usePriorityAttentionStore.setState({ cue: "reminded" });
    const { result } = renderHook(() => usePriorityAttentionClass(true));
    expect(result.current).toBe("priority-attention-glow priority-attention-glow-escalated");
  });

  it("is empty when the viewer cannot act (the gate that keeps another seat's window silent)", () => {
    const { result } = renderHook(() => usePriorityAttentionClass(false));
    expect(result.current).toBe("");
  });

  it("is empty when the glow preference is off", () => {
    usePreferencesStore.setState({ priorityAlertVisual: false });
    const { result } = renderHook(() => usePriorityAttentionClass(true));
    expect(result.current).toBe("");
  });

  it("is empty when nothing is announced", () => {
    usePriorityAttentionStore.setState({ cue: "none" });
    const { result } = renderHook(() => usePriorityAttentionClass(true));
    expect(result.current).toBe("");
  });
});
