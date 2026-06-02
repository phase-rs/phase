/**
 * Issue #1513 — P0 softlock with Ancient Greenwarden (doubled landfall triggers).
 *
 * Ancient Greenwarden's `DoubleTriggers` static ability causes every landfall
 * trigger to fire twice. With Ob Nixilis on the battlefield and a land played,
 * two `OptionalEffectChoice{player:0, source_id:OB_NIXILIS_ID}` prompts appear
 * in sequence — structurally identical (same type, same player, same source_id,
 * same description) but against different engine states (stack 1 vs stack 0).
 *
 * Root cause: `dispatchAction`'s in-flight and queued de-dup keyed on
 * `{ action, actor }` alone. When the second `DecideOptionalEffect{accept:true}`
 * arrives while the first is in-flight (or still in the `pendingQueue`), the
 * de-dup wrongly treats it as a duplicate of the first and silently drops it.
 *
 * The race surface: with `animationSpeedMultiplier > 0`, an in-flight
 * processAction holds the `isAnimating` mutex for the duration of its animation
 * timer. If the engine state advances through processQueue (updating the store
 * `waitingFor` between processAction awaits), a subsequent dispatch of the same
 * action+actor pair against the NEW state enters the de-dup check with a stale
 * `inFlightLocalAction.waitingFor`. Without the state-token check the second
 * dispatch is silently dropped.
 *
 * Fix: include the `WaitingFor` reference in the de-dup identity. Two dispatches
 * with the same `{action, actor}` but different `waitingFor` references are
 * responses to different engine prompts and must NOT be collapsed.
 *
 * Test strategy: construct the exact collision synthetically — dispatch an
 * action while a "slow" in-flight processAction holds the mutex, then advance
 * the store `waitingFor` to a new object (simulating processQueue's mid-run
 * setState), then dispatch the same action+actor again. Before the fix: second
 * dispatch is dropped. After the fix: second dispatch goes through.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "@testing-library/react";

import type {
  EngineAdapter,
  GameAction,
  GameState,
  LegalActionsResult,
  SubmitResult,
  WaitingFor,
} from "../../adapter/types";
import { dispatchAction } from "../../game/dispatch";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useUiStore } from "../../stores/uiStore";

// ── Fixtures ─────────────────────────────────────────────────────────────

const OB_NIXILIS_ID = 100;
const PLAYER_ID = 0;

const OEC_1: WaitingFor = {
  type: "OptionalEffectChoice",
  data: { player: PLAYER_ID, source_id: OB_NIXILIS_ID, description: "Ob Nixilis — lose 3 life" },
};

const OEC_2: WaitingFor = {
  type: "OptionalEffectChoice",
  data: { player: PLAYER_ID, source_id: OB_NIXILIS_ID, description: "Ob Nixilis — lose 3 life" },
};

// Verify the two WaitingFor objects are structurally identical but different references.
// If they were the same reference, the fix would be moot and this test trivially passes.
if (Object.is(OEC_1, OEC_2)) {
  throw new Error("Test setup error: OEC_1 and OEC_2 must be different references");
}

function baseState(waitingFor: WaitingFor): GameState {
  return {
    turn_number: 3,
    active_player: PLAYER_ID,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 20, turns_taken: 1 } as unknown as GameState["players"][number],
    ],
    priority_player: PLAYER_ID,
    objects: {
      [OB_NIXILIS_ID]: { name: "Ob Nixilis, the Fallen" } as unknown as GameState["objects"][number],
    },
    next_object_id: 300,
    battlefield: [OB_NIXILIS_ID],
    stack: [],
    exile: [],
    rng_seed: 42,
    combat: null,
    waiting_for: waitingFor,
    has_pending_cast: false,
    lands_played_this_turn: 1,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
    turn_decision_controller: PLAYER_ID,
    phase_stops: {},
  } as unknown as GameState;
}

const DECIDE_OPTIONAL: GameAction = {
  type: "DecideOptionalEffect",
  data: { accept: true },
} as unknown as GameAction;

/** Flush pending microtasks (Promise resolutions). */
async function flushMicrotasks(): Promise<void> {
  for (let i = 0; i < 20; i++) {
    await Promise.resolve();
  }
}

// ── Test suite ────────────────────────────────────────────────────────────

describe("issue #1513 — state-token de-dup (Greenwarden doubled OptionalEffectChoice)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // Real-world animation speed: processAction will await a setTimeout
    usePreferencesStore.setState({ animationSpeedMultiplier: 1.0 });
    useUiStore.setState({ pendingAbilityChoice: null, enchantmentsDialogPlayer: null });
  });

  afterEach(() => {
    vi.useRealTimers();
    act(() => {
      useGameStore.setState({ gameState: null, waitingFor: null, adapter: null });
    });
  });

  /**
   * Discriminating test for the state-token de-dup fix.
   *
   * Scenario: the `isAnimating` mutex is held by a slow processAction (400 ms
   * animation). During the animation window, the store `waitingFor` transitions
   * from `OEC_1` to `OEC_2` (simulating what processQueue does when it runs
   * intermediate items between awaits). A `DecideOptionalEffect{P0}` dispatch
   * then arrives against `OEC_2`.
   *
   * Before the fix: the de-dup matches on `{action, actor}` alone; since the
   * same `DecideOptionalEffect{P0}` from the first dispatch is already
   * `inFlightLocalAction`, the second dispatch is silently dropped.
   *
   * After the fix: the de-dup also compares `waitingFor` references;
   * `OEC_1 !== OEC_2` (different objects), so the second dispatch is NOT
   * suppressed.
   */
  it("does NOT suppress the second DecideOptionalEffect when the WaitingFor reference changed (issue #1513)", async () => {
    const delivered: { action: GameAction; actor: number }[] = [];

    // Slow adapter: submitAction returns a LifeChanged event so processAction
    // will await a 300 ms animation timer, keeping isAnimating=true long
    // enough for the test to dispatch the second action in the mutex window.
    const adapter: EngineAdapter = {
      initialize: vi.fn().mockResolvedValue(undefined),
      initializeGame: vi.fn().mockResolvedValue({ events: [] } as SubmitResult),
      submitAction: vi.fn(async (action: GameAction, actor: number): Promise<SubmitResult> => {
        delivered.push({ action, actor });
        // Return a LifeChanged event → normalizeEvents produces a 300 ms step
        return {
          events: [
            {
              type: "LifeChanged",
              data: { player_id: PLAYER_ID, old_life: 20, new_life: 17 },
            } as unknown as import("../../adapter/types").GameEvent,
          ],
        };
      }),
      getState: vi.fn(async () => baseState(OEC_2)),  // after first action: engine is at OEC_2
      getLegalActions: vi.fn(async (): Promise<LegalActionsResult> => ({
        actions: [],
        autoPassRecommended: false,
      })),
      restoreState: vi.fn(),
      getAiAction: vi.fn().mockReturnValue(null),
      estimateBracket: vi.fn().mockResolvedValue(null),
      dispose: vi.fn(),
    };

    // Seed the store with the FIRST OptionalEffectChoice.
    act(() => {
      useGameStore.setState({
        gameId: null,
        gameMode: "ai",
        adapter,
        gameState: baseState(OEC_1),
        waitingFor: OEC_1,
        events: [],
        eventHistory: [],
        logHistory: [],
        nextLogSeq: 0,
        stateHistory: [],
        turnCheckpoints: [],
        legalActions: [],
        autoPassRecommended: false,
      });
    });

    // ── Phase 1: dispatch the first DecideOptionalEffect against OEC_1 ──
    // isAnimating=false → fast path, sets inFlightLocalAction={DECIDE_OPTIONAL, P0, OEC_1}
    // processAction starts: submitAction resolves (microtask), animation timer 300 ms set.
    const firstDispatch = dispatchAction(DECIDE_OPTIONAL, PLAYER_ID);
    // Flush microtasks so submitAction and getState resolve; the 300 ms animation
    // timer is now pending.
    await flushMicrotasks();

    // isAnimating=true, inFlightLocalAction is set. The 300 ms animation timer
    // is waiting. Now simulate the store waitingFor advancing to OEC_2 (as would
    // happen in the real game when processQueue runs intermediate items while
    // this processAction animates).
    act(() => {
      useGameStore.setState({ waitingFor: OEC_2, gameState: baseState(OEC_2) });
    });

    // ── Phase 2: dispatch the second DecideOptionalEffect against OEC_2 ──
    // isAnimating=true → enters the de-dup path.
    // currentWaitingFor = OEC_2 (just set above).
    // inFlightLocalAction.waitingFor = OEC_1 (from Phase 1).
    //
    // PRE-FIX:  Object.is(OEC_1, OEC_2) = false is NOT checked — only
    //           {action, actor} are compared → match → DROPPED.
    // POST-FIX: Object.is(OEC_1, OEC_2) = false → NOT suppressed → queued.
    const secondDispatch = dispatchAction(DECIDE_OPTIONAL, PLAYER_ID);
    await flushMicrotasks();

    // ── Phase 3: let both animations complete ──
    // Advance 350 ms to fire the 300 ms animation timer for the first dispatch.
    await vi.advanceTimersByTimeAsync(350);
    await flushMicrotasks();
    await firstDispatch;

    // If the second dispatch was queued (not dropped), it runs now via processQueue.
    // Its processAction also has a 300 ms animation.
    await vi.advanceTimersByTimeAsync(350);
    await flushMicrotasks();
    await secondDispatch;

    // ── Assertion ──
    // Both DecideOptionalEffect actions must reach the engine.
    // Before the fix: only 1 delivered (the second was silently dropped).
    // After the fix: 2 delivered.
    expect(delivered).toHaveLength(2);
    expect(delivered[0]).toMatchObject({ action: { type: "DecideOptionalEffect" }, actor: PLAYER_ID });
    expect(delivered[1]).toMatchObject({ action: { type: "DecideOptionalEffect" }, actor: PLAYER_ID });
  });

  /**
   * Preservation test: genuine double-clicks on the SAME WaitingFor reference
   * must still be suppressed (the original de-dup guarantee).
   */
  it("DOES suppress a genuine double-click on the same WaitingFor reference", async () => {
    const delivered: { action: GameAction; actor: number }[] = [];

    const adapter: EngineAdapter = {
      initialize: vi.fn().mockResolvedValue(undefined),
      initializeGame: vi.fn().mockResolvedValue({ events: [] } as SubmitResult),
      submitAction: vi.fn(async (action: GameAction, actor: number): Promise<SubmitResult> => {
        delivered.push({ action, actor });
        return {
          events: [
            {
              type: "LifeChanged",
              data: { player_id: PLAYER_ID, old_life: 20, new_life: 17 },
            } as unknown as import("../../adapter/types").GameEvent,
          ],
        };
      }),
      getState: vi.fn(async () => baseState(OEC_1)),
      getLegalActions: vi.fn(async (): Promise<LegalActionsResult> => ({
        actions: [],
        autoPassRecommended: false,
      })),
      restoreState: vi.fn(),
      getAiAction: vi.fn().mockReturnValue(null),
      estimateBracket: vi.fn().mockResolvedValue(null),
      dispose: vi.fn(),
    };

    act(() => {
      useGameStore.setState({
        gameId: null,
        gameMode: "ai",
        adapter,
        gameState: baseState(OEC_1),
        waitingFor: OEC_1,
        events: [],
        eventHistory: [],
        logHistory: [],
        nextLogSeq: 0,
        stateHistory: [],
        turnCheckpoints: [],
        legalActions: [],
        autoPassRecommended: false,
      });
    });

    // First dispatch — sets inFlightLocalAction with waitingFor=OEC_1
    const firstDispatch = dispatchAction(DECIDE_OPTIONAL, PLAYER_ID);
    await flushMicrotasks();

    // Second dispatch — same action, same actor, SAME waitingFor reference (OEC_1)
    // This is a genuine double-click; it must be suppressed.
    // The store waitingFor is still OEC_1 (not changed between dispatches).
    const secondDispatch = dispatchAction(DECIDE_OPTIONAL, PLAYER_ID);
    await flushMicrotasks();

    // Let both complete
    await vi.advanceTimersByTimeAsync(400);
    await flushMicrotasks();
    await firstDispatch;
    await secondDispatch; // resolves immediately (was dropped)

    // Only one action should reach the engine — the second was a double-click.
    expect(delivered).toHaveLength(1);
  });
});
