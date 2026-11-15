/**
 * Remote-update queue is bounded, and a backgrounded tab does not stall on timers.
 *
 * Observed: in a long multi-seat game, a hidden tab throttles `setTimeout` to
 * >= 1 s (about once a minute when intensively throttled) while the server
 * keeps pushing updates. Every update that arrived during an animation was
 * queued with its full engine snapshot, so the queue grew without bound until
 * the browser discarded the tab, and the board was minutes stale on return.
 *
 * Fix under test: adjacent queued remote updates are coalesced into one entry
 * (newest-by-seq snapshot, events/log entries concatenated in arrival order),
 * and animation waits are skipped while the document is hidden or the client is
 * far behind.
 *
 * Commit counts show how many queue entries ran (each commits once); they are
 * not memory evidence. Snapshot retention is checked directly by test (f),
 * which needs `--expose-gc` (e.g. `NODE_OPTIONS=--expose-gc`) and is skipped
 * otherwise.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  EngineAdapter,
  EngineSnapshot,
  GameAction,
  GameEvent,
  GameLogEntry,
  SubmitResult,
} from "../../adapter/types";
import { nextSnapshotSeq } from "../../adapter/types";
import { normalizeEvents } from "../../animation/eventNormalizer";
import { audioManager } from "../../audio/AudioManager";
import { useAnimationStore } from "../../stores/animationStore";
import { useGameStore } from "../../stores/gameStore";
import { useUiStore } from "../../stores/uiStore";
import { getPlayerId } from "../../hooks/usePlayerId";
import { usePreferencesStore } from "../../stores/preferencesStore";
import {
  buildGameState,
  buildLegalActionsResult,
  buildPriorityWaitingFor,
} from "../../test/factories/gameStateFactory";
import { dispatchAction, isDispatchIdle, processRemoteUpdate } from "../dispatch";

const PASS = { type: "PassPriority" } as unknown as GameAction;

/** A priority snapshot tagged by `turn`, stamped with the next global seq. */
function snapshot(turn: number): EngineSnapshot {
  return {
    state: buildGameState({
      waiting_for: buildPriorityWaitingFor({ data: { player: 0 } }),
      turn_number: turn,
    }),
    legalResult: buildLegalActionsResult({ actions: [PASS] }),
    seq: nextSnapshotSeq(),
  };
}

/** An event that normalizes to a real (timed) animation step; `amount` tags it. */
function lifeEvent(amount = -1): GameEvent[] {
  return [{ type: "LifeChanged", data: { player_id: 1, amount } }];
}

/** An event that always starts its own animation step (and flashes the turn banner). */
function turnStarted(playerId: number): GameEvent {
  return { type: "TurnStarted", data: { player_id: playerId, turn_number: 2 } };
}

/** The `amount` tag of every committed `LifeChanged`, in history order. */
function lifeAmounts(): number[] {
  return useGameStore
    .getState()
    .eventHistory.flatMap((event) => (event.type === "LifeChanged" ? [event.data.amount] : []));
}

function logEntry(label: string): GameLogEntry {
  return {
    seq: 0,
    turn: 1,
    phase: "PreCombatMain",
    category: "Debug",
    segments: [{ type: "Text", value: label }],
  };
}

/** The test-authored log labels; `debugLog` also writes "[WARN] ..." lines into the log. */
function logLabels(): string[] {
  return useGameStore
    .getState()
    .logHistory.map((entry) => (entry.segments[0].type === "Text" ? entry.segments[0].value : "?"))
    .filter((label) => !label.startsWith("[WARN]"));
}

function setDocumentHidden(hidden: boolean): void {
  Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
}

/** Wrap `commitEngineSnapshot` and record the turn of every snapshot it receives. */
function recordCommits(): number[] {
  const turns: number[] = [];
  const original = useGameStore.getState().commitEngineSnapshot;
  useGameStore.setState({
    commitEngineSnapshot: (snap, opts) => {
      turns.push(snap.state.turn_number);
      return original(snap, opts);
    },
  });
  return turns;
}

// `vi.spyOn` on a zustand store method only patches the current state object;
// every later `set` copies the spy into the next one, out of reach of
// `vi.restoreAllMocks`. The originals are reinstalled after each test instead.
const originalEnqueueSteps = useAnimationStore.getState().enqueueSteps;
const originalFlashTurnBanner = useUiStore.getState().flashTurnBanner;
const originalFlashDiceRoll = useUiStore.getState().flashDiceRoll;

describe("processRemoteUpdate queue coalescing", () => {
  let originalCommit: ReturnType<typeof useGameStore.getState>["commitEngineSnapshot"];

  beforeEach(() => {
    useGameStore.getState().reset();
    originalCommit = useGameStore.getState().commitEngineSnapshot;
    usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
    setDocumentHidden(false);
    vi.useFakeTimers();
  });

  afterEach(async () => {
    await vi.runAllTimersAsync();
    useGameStore.setState({ commitEngineSnapshot: originalCommit });
    useAnimationStore.setState({ enqueueSteps: originalEnqueueSteps });
    useUiStore.setState({
      flashTurnBanner: originalFlashTurnBanner,
      flashDiceRoll: originalFlashDiceRoll,
    });
    setDocumentHidden(false);
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it("(a) coalesces 50 updates delivered during an animation into at most 2 retained entries", async () => {
    const commits = recordCommits();

    // Update 1 starts animating immediately and holds the mutex on a timer.
    const settled: number[] = [];
    const calls: Promise<void>[] = [];
    for (let i = 1; i <= 50; i++) {
      calls.push(
        processRemoteUpdate(snapshot(i), lifeEvent(-i), [logEntry(`log-${i}`)]).then(() => {
          settled.push(i);
        }),
      );
    }
    await vi.advanceTimersByTimeAsync(0);
    expect(commits).toEqual([]); // still animating: nothing committed yet

    await vi.runAllTimersAsync();
    await Promise.all(calls);

    // In-flight update + ONE merged entry for the other 49 — not 50 entries.
    expect(commits.length).toBeLessThanOrEqual(2);
    expect(useGameStore.getState().gameState?.turn_number).toBe(50);
    expect(logLabels()).toEqual(Array.from({ length: 50 }, (_, i) => `log-${i + 1}`));
    // No event lost or duplicated by the merge, and history keeps arrival order.
    expect(lifeAmounts()).toEqual(Array.from({ length: 50 }, (_, i) => -(i + 1)));
    // Every caller's promise resolved, in arrival order.
    expect(settled).toEqual(Array.from({ length: 50 }, (_, i) => i + 1));
    expect(isDispatchIdle()).toBe(true);
  });

  it("(e) settles 20,000 merged callers without unbounded recursion", async () => {
    let resolved = 0;
    const calls = Array.from({ length: 20_000 }, (_, i) =>
      processRemoteUpdate(snapshot(i + 1), [], []).then(() => {
        resolved += 1;
      }),
    );
    await vi.runAllTimersAsync();
    await Promise.all(calls);
    expect(resolved).toBe(20_000);
    expect(useGameStore.getState().gameState?.turn_number).toBe(20_000);
  });

  it("(j) merges large non-empty sidecar batches in order without mutating callers' arrays", async () => {
    const first = lifeEvent();
    const firstLog = [logEntry("log-1")];
    const calls: Promise<void>[] = [processRemoteUpdate(snapshot(1), lifeEvent(), [logEntry("log-0")])];
    calls.push(processRemoteUpdate(snapshot(2), first, firstLog));
    for (let i = 3; i <= 2_000; i++) {
      calls.push(processRemoteUpdate(snapshot(i), lifeEvent(), [logEntry(`log-${i - 1}`)]));
    }
    await vi.runAllTimersAsync();
    await Promise.all(calls);
    expect(first).toHaveLength(1);
    expect(firstLog).toHaveLength(1);
    expect(logLabels()).toEqual(Array.from({ length: 2_000 }, (_, i) => `log-${i}`));
  });

  // Needs `--expose-gc` (e.g. NODE_OPTIONS=--expose-gc); a plain run skips it.
  // WeakRef is outside this project's TS lib, so it is reached through a narrow local type.
  const gc = (globalThis as { gc?: () => void }).gc;
  const WeakRefCtor = (globalThis as {
    WeakRef?: new (target: object) => { deref(): object | undefined };
  }).WeakRef;
  it.runIf(typeof gc === "function" && WeakRefCtor !== undefined)(
    "(f) does not retain superseded snapshots while their merged entry waits",
    async () => {
      // Update 1 holds the mutex on its animation timer; 2..101 merge into one entry.
      const calls: Promise<void>[] = [processRemoteUpdate(snapshot(1), lifeEvent(), [])];
      const refs: { deref(): object | undefined }[] = [];
      for (let i = 2; i <= 101; i++) {
        const snap = snapshot(i);
        refs.push(new WeakRefCtor!(snap));
        calls.push(processRemoteUpdate(snap, lifeEvent(), []));
      }
      await vi.advanceTimersByTimeAsync(0);
      for (let i = 0; i < 5; i++) {
        gc!();
        await Promise.resolve();
      }
      // Only the newest (winning) snapshot may still be alive while queued.
      const alive = refs.filter((ref) => ref.deref() !== undefined).length;
      expect(alive).toBeLessThanOrEqual(1);
      await vi.runAllTimersAsync();
      await Promise.all(calls);
    },
  );

  it("(g) releases an animation wait already in progress when the tab becomes hidden", async () => {
    const running = processRemoteUpdate(snapshot(1), lifeEvent(), []);
    await vi.advanceTimersByTimeAsync(0);
    expect(vi.getTimerCount()).toBeGreaterThan(0); // animating, not committed yet
    expect(useGameStore.getState().gameState?.turn_number).not.toBe(1);

    setDocumentHidden(true);
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(0); // no animation time elapses
    await running;
    expect(useGameStore.getState().gameState?.turn_number).toBe(1);
  });

  it("(g2) hiding mid-animation clears the queued animation steps and pending step SFX", async () => {
    useAnimationStore.getState().clearQueue();
    const sfx = vi.spyOn(audioManager, "playSfxForStep");
    // Several steps, so later steps' SFX are scheduled on timers (the first plays at once).
    const steps: GameEvent[] = [...lifeEvent(), turnStarted(1), ...lifeEvent(), turnStarted(0)];
    expect(normalizeEvents(steps).length).toBeGreaterThanOrEqual(2);
    const running = processRemoteUpdate(snapshot(1), steps, []);
    await vi.advanceTimersByTimeAsync(0);
    expect(useAnimationStore.getState().activeStep).not.toBeNull();
    const playedBeforeHide = sfx.mock.calls.length;
    expect(playedBeforeHide).toBe(1); // only the first step's SFX so far; the rest wait on timers

    setDocumentHidden(true);
    document.dispatchEvent(new Event("visibilitychange"));
    await running;
    expect(useAnimationStore.getState().queue).toEqual([]);
    expect(useAnimationStore.getState().activeStep).toBeNull();
    await vi.runAllTimersAsync(); // no stale step SFX fires later
    expect(sfx.mock.calls.length).toBe(playedBeforeHide);
  });

  it("(h) a skipped catch-up batch queues no per-event dice notices", async () => {
    setDocumentHidden(true);
    const flash = vi.spyOn(useUiStore.getState(), "flashDiceRoll");
    const coin: GameEvent[] = [{ type: "CoinFlipped", data: { player_id: 1, won: true } }];
    const calls = Array.from({ length: 20 }, (_, i) => processRemoteUpdate(snapshot(i + 1), coin, []));
    await vi.advanceTimersByTimeAsync(0);
    await Promise.all(calls);
    expect(flash).not.toHaveBeenCalled();
    expect(useGameStore.getState().gameState?.turn_number).toBe(20);
  });

  it("(b) does not wait on animation timers while the document is hidden", async () => {
    setDocumentHidden(true);
    const sfx = vi.spyOn(audioManager, "playSfxForStep");
    const commits = recordCommits();

    const calls = Array.from({ length: 20 }, (_, i) =>
      processRemoteUpdate(snapshot(i + 1), lifeEvent(), [logEntry(`log-${i + 1}`)]),
    );
    // Flush microtasks only — fake timers are NOT advanced by any animation duration.
    await vi.advanceTimersByTimeAsync(0);
    await Promise.all(calls);

    expect(vi.getTimerCount()).toBe(0);
    expect(useGameStore.getState().gameState?.turn_number).toBe(20);
    expect(logLabels()).toEqual(Array.from({ length: 20 }, (_, i) => `log-${i + 1}`));
    expect(commits.length).toBeLessThanOrEqual(2);
    expect(sfx).not.toHaveBeenCalled();
    expect(isDispatchIdle()).toBe(true);
  });

  it("(b2) resumes normal animations for new updates once the document is visible again", async () => {
    setDocumentHidden(true);
    await processRemoteUpdate(snapshot(1), lifeEvent(), []);
    expect(vi.getTimerCount()).toBe(0);

    setDocumentHidden(false);
    const visible = processRemoteUpdate(snapshot(2), lifeEvent(), []);
    await vi.advanceTimersByTimeAsync(0);
    expect(vi.getTimerCount()).toBeGreaterThan(0); // animating again
    expect(useGameStore.getState().gameState?.turn_number).toBe(1);

    await vi.runAllTimersAsync();
    await visible;
    expect(useGameStore.getState().gameState?.turn_number).toBe(2);
  });

  it("(k) a visible client animates an unmerged update but commits any merged batch without animation", async () => {
    const enqueue = vi.spyOn(useAnimationStore.getState(), "enqueueSteps");
    const sfx = vi.spyOn(audioManager, "playSfxForStep");

    // A single queued update behind the in-flight one is not merged: both animate.
    const single = [1, 2].map((i) => processRemoteUpdate(snapshot(i), lifeEvent(-i), []));
    await vi.runAllTimersAsync();
    await Promise.all(single);
    expect(enqueue).toHaveBeenCalledTimes(2);
    expect(sfx).toHaveBeenCalledTimes(2);

    // In flight + a merge of 2: the merged batch carries only the later snapshot,
    // so it commits without animation or SFX (only the in-flight update animates).
    enqueue.mockClear();
    sfx.mockClear();
    const merged = [3, 4, 5].map((i) => processRemoteUpdate(snapshot(i), lifeEvent(-i), []));
    await vi.runAllTimersAsync();
    await Promise.all(merged);
    expect(enqueue).toHaveBeenCalledTimes(1);
    expect(sfx).toHaveBeenCalledTimes(1);
    expect(useGameStore.getState().gameState?.turn_number).toBe(5);
    expect(lifeAmounts()).toEqual([-1, -2, -3, -4, -5]);
  });

  it("(l) a merged batch spanning several turn starts flashes only the latest turn's banner", async () => {
    const me = getPlayerId();
    const opponent = me === 0 ? 1 : 0;
    const banner = vi.spyOn(useUiStore.getState(), "flashTurnBanner");
    const calls = [
      processRemoteUpdate(snapshot(1), lifeEvent(), []), // in flight
      processRemoteUpdate(snapshot(2), [turnStarted(opponent)], []),
      processRemoteUpdate(snapshot(3), [turnStarted(me)], []),
    ];
    await vi.runAllTimersAsync();
    await Promise.all(calls);

    expect(banner).toHaveBeenCalledTimes(1);
    expect(banner.mock.calls[0][0]).toBe("YOUR TURN");
  });

  it("(c) never reorders a remote update across a queued local action", async () => {
    const r1 = snapshot(1);
    const localSnap = snapshot(2);
    const r2 = snapshot(3);
    const r3 = snapshot(4);
    const commits = recordCommits();

    const adapter = {
      initialize: vi.fn(),
      initializeGame: vi.fn(),
      restoreState: vi.fn(),
      estimateBracket: vi.fn(),
      dispose: vi.fn(),
      submitAction: vi.fn(async (): Promise<SubmitResult> => ({
        events: [],
        log_entries: [logEntry("local")],
      })),
      getSnapshot: vi.fn(async () => localSnap),
    } as unknown as EngineAdapter;
    useGameStore.setState({
      gameId: null,
      gameMode: "ai",
      adapter,
      gameState: buildGameState({ waiting_for: buildPriorityWaitingFor({ data: { player: 0 } }) }),
      waitingFor: buildPriorityWaitingFor({ data: { player: 0 } }),
      legalActions: [PASS],
    });

    const calls = [
      processRemoteUpdate(r1, lifeEvent(), [logEntry("r1")]), // in flight (animating)
      dispatchAction(PASS, 0), // queued local
      processRemoteUpdate(r2, lifeEvent(), [logEntry("r2")]), // queued after the local
      processRemoteUpdate(r3, lifeEvent(), [logEntry("r3")]), // coalesces with r2 only
    ];
    await vi.runAllTimersAsync();
    await Promise.all(calls);

    expect(adapter.submitAction).toHaveBeenCalledTimes(1);
    // r1, then the local action's snapshot, then ONE merged entry (r2+r3): the
    // two remotes were not merged across the local action, and not reordered.
    expect(commits).toEqual([1, 2, 4]);
    expect(logLabels()).toEqual(["r1", "local", "r2", "r3"]);
    expect(useGameStore.getState().gameState?.turn_number).toBe(4);
  });

  it("(i) a hidden local action queues no animation steps and does not wait on timers", async () => {
    setDocumentHidden(true);
    const enqueue = vi.spyOn(useAnimationStore.getState(), "enqueueSteps");
    const flash = vi.spyOn(useUiStore.getState(), "flashDiceRoll");
    const adapter = {
      initialize: vi.fn(),
      initializeGame: vi.fn(),
      restoreState: vi.fn(),
      estimateBracket: vi.fn(),
      dispose: vi.fn(),
      submitAction: vi.fn(async (): Promise<SubmitResult> => ({
        events: [...lifeEvent(), { type: "CoinFlipped", data: { player_id: 0, won: true } }],
        log_entries: [logEntry("local")],
      })),
      getSnapshot: vi.fn(async () => snapshot(2)),
    } as unknown as EngineAdapter;
    useGameStore.setState({
      gameId: null,
      gameMode: "ai",
      adapter,
      gameState: buildGameState({ waiting_for: buildPriorityWaitingFor({ data: { player: 0 } }) }),
      waitingFor: buildPriorityWaitingFor({ data: { player: 0 } }),
      legalActions: [PASS],
    });

    const local = dispatchAction(PASS, 0);
    await vi.advanceTimersByTimeAsync(0); // no animation time elapses
    await local;

    expect(adapter.submitAction).toHaveBeenCalledTimes(1);
    expect(enqueue).not.toHaveBeenCalled();
    expect(flash).not.toHaveBeenCalled();
    expect(useGameStore.getState().gameState?.turn_number).toBe(2);
  });

  it("(d) keeps revision gating: an older-seq update merged after a newer one never wins", async () => {
    const blocker = snapshot(1);
    const older = snapshot(10);
    const newer = snapshot(11);
    // seq order: blocker (animates) < older < newer; arrival order: newer, then older.
    const calls = [
      processRemoteUpdate(blocker, lifeEvent(), [logEntry("blocker")]),
      processRemoteUpdate(newer, lifeEvent(), [logEntry("newer")]),
      processRemoteUpdate(older, lifeEvent(), [logEntry("older")]),
    ];
    await vi.runAllTimersAsync();
    await Promise.all(calls);

    expect(useGameStore.getState().gameState?.turn_number).toBe(11);
    expect(useGameStore.getState().lastCommittedSeq).toBe(newer.seq);
    // History still reflects arrival order, nothing lost.
    expect(logLabels()).toEqual(["blocker", "newer", "older"]);
  });

  it("(d2) rewind targets follow the winning snapshot, and undefined never erases a published list", async () => {
    const targets = [{ turn_number: 3, active_player: 1 }];
    const calls = [
      processRemoteUpdate(snapshot(1), lifeEvent(), [], undefined), // in flight
      processRemoteUpdate(snapshot(2), lifeEvent(), [], targets),
      processRemoteUpdate(snapshot(3), lifeEvent(), [], undefined),
    ];
    await vi.runAllTimersAsync();
    await Promise.all(calls);

    expect(useGameStore.getState().rewindTargets).toEqual(targets);
  });

  it("(d3) an older-seq update merged after a newer one does not replace the newer one's rewind targets", async () => {
    const blocker = snapshot(1);
    const older = snapshot(2);
    const newer = snapshot(3);
    const newerTargets = [{ turn_number: 3, active_player: 1 }];
    const olderTargets = [{ turn_number: 2, active_player: 0 }];
    const calls = [
      processRemoteUpdate(blocker, lifeEvent(), [], undefined), // in flight
      processRemoteUpdate(newer, lifeEvent(), [], newerTargets),
      processRemoteUpdate(older, lifeEvent(), [], olderTargets),
    ];
    await vi.runAllTimersAsync();
    await Promise.all(calls);

    expect(useGameStore.getState().lastCommittedSeq).toBe(newer.seq);
    expect(useGameStore.getState().rewindTargets).toEqual(newerTargets);
  });

  it("rejects every coalesced caller together when the merged entry fails", async () => {
    const boom = new Error("commit failed");
    const original = useGameStore.getState().commitEngineSnapshot;
    let n = 0;
    useGameStore.setState({
      commitEngineSnapshot: (snap, opts) => {
        if (++n === 2) throw boom;
        return original(snap, opts);
      },
    });

    const first = processRemoteUpdate(snapshot(1), lifeEvent(), []);
    const second = processRemoteUpdate(snapshot(2), lifeEvent(), []);
    const third = processRemoteUpdate(snapshot(3), lifeEvent(), []);
    const outcomes = Promise.allSettled([first, second, third]);
    await vi.runAllTimersAsync();

    const results = await outcomes;
    expect(results.map((r) => r.status)).toEqual(["fulfilled", "rejected", "rejected"]);
  });
});
