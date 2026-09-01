import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { EngineAdapter, EngineSnapshot, GameState, LegalActionsResult } from "../../adapter/types";
import { nextSnapshotSeq } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { debugLog } from "../debugLog";
import { processRemoteUpdate } from "../dispatch";
import {
  WATCHDOG_ARM_DELAY_MS,
  createStaleStateWatchdog,
  resyncFromAdapter,
  resyncFromAdapterSafely,
  stateFingerprint,
} from "../staleStateWatchdog";

// Failure injection for the commit pipeline: when set, every
// `processRemoteUpdate` rejects with this error instead of committing.
const harness = vi.hoisted(() => ({
  failRemoteUpdate: null as Error | null,
}));

vi.mock("../dispatch", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../dispatch")>();
  return {
    ...actual,
    processRemoteUpdate: (
      ...args: Parameters<typeof actual.processRemoteUpdate>
    ) =>
      harness.failRemoteUpdate
        ? Promise.reject(harness.failRemoteUpdate)
        : actual.processRemoteUpdate(...args),
  };
});

vi.mock("../debugLog", () => ({ debugLog: vi.fn() }));

function loggedLineIncluding(fragment: string): boolean {
  return vi
    .mocked(debugLog)
    .mock.calls.some(([message]) => String(message).includes(fragment));
}

// These tests prove the healing mechanism and its causality: each commit
// arms exactly one deferred check, a clean check disarms until the next
// commit (no polling), and a divergent check re-commits the adapter's
// snapshot. They do NOT prove the original pod-draft incident (a rejected
// delivery freezing the host on the mulligan overlay) end-to-end — that
// failure needs a real P2P match; the delivery `.catch` sites and the
// emit-before-fan-out/AI-loop reorder in `p2p-adapter.ts` cover it by
// construction.

function stateAt(turn: number, priorityPlayer: number): GameState {
  return {
    turn_number: turn,
    active_player: 0,
    phase: "PreCombatMain",
    players: [],
    priority_player: priorityPlayer,
    objects: {},
    next_object_id: 1,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 42,
    combat: null,
    waiting_for: { type: "Priority", data: { player: priorityPlayer } },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
  };
}

function noLegalActions(): LegalActionsResult {
  return { actions: [], autoPassRecommended: false };
}

function snapshotOf(state: GameState): EngineSnapshot {
  return { state, legalResult: noLegalActions(), seq: nextSnapshotSeq() };
}

/** Adapter stub: only `getSnapshot` is consulted by the watchdog. */
function stubAdapter(current: () => EngineSnapshot): EngineAdapter {
  return { getSnapshot: async () => current(), dispose: () => {} } as unknown as EngineAdapter;
}

/** Adapter whose `getSnapshot` stays pending until the test resolves it. */
function deferredAdapter(): {
  adapter: EngineAdapter;
  resolve: (s: EngineSnapshot) => void;
} {
  let resolveFn: (s: EngineSnapshot) => void = () => {};
  const pending = new Promise<EngineSnapshot>((res) => {
    resolveFn = res;
  });
  return {
    adapter: {
      getSnapshot: () => pending,
      dispose: () => {},
    } as unknown as EngineAdapter,
    resolve: resolveFn,
  };
}

async function elapse(ms: number): Promise<void> {
  await vi.advanceTimersByTimeAsync(ms);
}

function committedFingerprint(): string {
  return stateFingerprint(useGameStore.getState().gameState!);
}

describe("staleStateWatchdog", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useGameStore.getState().reset();
    harness.failRemoteUpdate = null;
    vi.mocked(debugLog).mockClear();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  async function commitScreenState(state: GameState): Promise<void> {
    await processRemoteUpdate(snapshotOf(state), []);
    expect(committedFingerprint()).toBe(stateFingerprint(state));
  }

  it("heals a divergence after the arm delay", async () => {
    const screen = stateAt(1, 0);
    const ahead = stateAt(2, 1);
    await commitScreenState(screen);
    useGameStore.setState({ adapter: stubAdapter(() => snapshotOf(ahead)) });

    const watchdog = createStaleStateWatchdog();
    watchdog.start();
    try {
      await elapse(WATCHDOG_ARM_DELAY_MS - 1);
      expect(committedFingerprint()).toBe(stateFingerprint(screen));
      await elapse(1);
      expect(committedFingerprint()).toBe(stateFingerprint(ahead));
    } finally {
      watchdog.stop();
    }
  });

  it("a clean check disarms — no polling until the next commit re-arms", async () => {
    const screen = stateAt(1, 0);
    const laterDivergence = stateAt(2, 1);
    // The stub stamps a fresh seq per read (like the host's engine does) —
    // a pre-built snapshot would lose to the commit gate on later reads.
    let currentState = screen;
    await commitScreenState(screen);
    useGameStore.setState({ adapter: stubAdapter(() => snapshotOf(currentState)) });

    const watchdog = createStaleStateWatchdog();
    watchdog.start();
    try {
      // First check finds agreement and disarms.
      await elapse(WATCHDOG_ARM_DELAY_MS);
      expect(committedFingerprint()).toBe(stateFingerprint(screen));
      // The adapter now diverges WITHOUT any commit event. A poller would
      // pick this up; the causal design must stay asleep.
      currentState = laterDivergence;
      await elapse(WATCHDOG_ARM_DELAY_MS * 5);
      expect(committedFingerprint()).toBe(stateFingerprint(screen));
      // The next commit re-arms, and that check heals. Fresh object of
      // equal content: the engine always delivers fresh state objects, and
      // the store's change notification keys on the reference.
      await commitScreenState(stateAt(1, 0));
      await elapse(WATCHDOG_ARM_DELAY_MS);
      expect(committedFingerprint()).toBe(stateFingerprint(laterDivergence));
    } finally {
      watchdog.stop();
    }
  });

  it("each commit replaces the pending check instead of stacking", async () => {
    const screen = stateAt(1, 0);
    const ahead = stateAt(2, 1);
    await commitScreenState(screen);
    useGameStore.setState({ adapter: stubAdapter(() => snapshotOf(ahead)) });

    const watchdog = createStaleStateWatchdog();
    watchdog.start();
    try {
      // A commit half-way through the delay restarts the clock: the check
      // may only fire a full quiet delay after the LAST commit.
      await elapse(WATCHDOG_ARM_DELAY_MS / 2);
      await commitScreenState(stateAt(1, 0)); // fresh object, same content
      await elapse(WATCHDOG_ARM_DELAY_MS / 2);
      expect(committedFingerprint()).toBe(stateFingerprint(screen));
      await elapse(WATCHDOG_ARM_DELAY_MS / 2);
      expect(committedFingerprint()).toBe(stateFingerprint(ahead));
    } finally {
      watchdog.stop();
    }
  });

  it("resyncFromAdapter recommits the adapter snapshot unconditionally", async () => {
    const screen = stateAt(1, 0);
    const ahead = stateAt(2, 1);
    await commitScreenState(screen);

    // Positive knowledge (a delivery rejected) must recommit even when the
    // coarse fingerprint agrees — the lost update may live entirely outside
    // it (a land play changes just a hand and the battlefield). The store
    // commits by reference, so the swap is observable without the client
    // deriving game-state equality.
    const agreeing = snapshotOf(stateAt(1, 0));
    useGameStore.setState({ adapter: stubAdapter(() => agreeing) });
    await resyncFromAdapter("test: lost update outside the fingerprint");
    expect(useGameStore.getState().gameState).toBe(agreeing.state);

    const diverged = snapshotOf(ahead);
    useGameStore.setState({ adapter: stubAdapter(() => diverged) });
    await resyncFromAdapter("test: divergence");
    expect(committedFingerprint()).toBe(stateFingerprint(ahead));
  });

  it("a rejected deferred check logs, re-arms, and heals after recovery", async () => {
    const screen = stateAt(1, 0);
    const ahead = stateAt(2, 1);
    await commitScreenState(screen);
    useGameStore.setState({ adapter: stubAdapter(() => snapshotOf(ahead)) });

    const watchdog = createStaleStateWatchdog();
    watchdog.start();
    try {
      harness.failRemoteUpdate = new Error("commit pipeline down");
      await elapse(WATCHDOG_ARM_DELAY_MS);
      // The recommit rejected: nothing committed, nothing escaped unhandled …
      expect(committedFingerprint()).toBe(stateFingerprint(screen));
      // … and the failure is on the record.
      expect(loggedLineIncluding("watchdog check failed")).toBe(true);
      // No commit happened, so no store subscription fired — only the
      // rejection path's own re-arm can drive this heal.
      harness.failRemoteUpdate = null;
      await elapse(WATCHDOG_ARM_DELAY_MS);
      expect(committedFingerprint()).toBe(stateFingerprint(ahead));
    } finally {
      watchdog.stop();
    }
  });

  it("stop() invalidates an in-flight check — no recommit after the lifecycle ended", async () => {
    const screen = stateAt(1, 0);
    const ahead = stateAt(2, 1);
    await commitScreenState(screen);
    const deferred = deferredAdapter();
    useGameStore.setState({ adapter: deferred.adapter });

    const watchdog = createStaleStateWatchdog();
    watchdog.start();
    // The check fires and is now awaiting the snapshot read …
    await elapse(WATCHDOG_ARM_DELAY_MS);
    // … the watchdog is stopped while that read is still in flight. The
    // adapter identity is UNCHANGED, so only the lifecycle guard can veto.
    watchdog.stop();
    deferred.resolve(snapshotOf(ahead));
    await elapse(0);
    expect(committedFingerprint()).toBe(stateFingerprint(screen));
  });

  it("resyncFromAdapter drops a snapshot from a replaced adapter", async () => {
    const screen = stateAt(1, 0);
    const ahead = stateAt(2, 1);
    await commitScreenState(screen);
    const oldAdapter = deferredAdapter();
    useGameStore.setState({ adapter: oldAdapter.adapter });

    const pending = resyncFromAdapter("test: adapter swapped mid-read");
    // The store swaps games while the old adapter's read is in flight; its
    // late snapshot must not be committed over the new game.
    useGameStore.setState({ adapter: stubAdapter(() => snapshotOf(screen)) });
    oldAdapter.resolve(snapshotOf(ahead));
    await pending;
    expect(committedFingerprint()).toBe(stateFingerprint(screen));
  });

  it("resyncFromAdapterSafely absorbs and logs a rejected resync", async () => {
    const screen = stateAt(1, 0);
    await commitScreenState(screen);
    useGameStore.setState({
      adapter: stubAdapter(() => snapshotOf(stateAt(2, 1))),
    });

    harness.failRemoteUpdate = new Error("commit pipeline down");
    resyncFromAdapterSafely("test: rejected resync");
    await elapse(0); // flush the rejection's microtask chain
    expect(loggedLineIncluding("stale-screen resync failed")).toBe(true);
    expect(committedFingerprint()).toBe(stateFingerprint(screen));
  });

  it("does nothing without an adapter or committed state", async () => {
    const watchdog = createStaleStateWatchdog();
    watchdog.start();
    try {
      await elapse(WATCHDOG_ARM_DELAY_MS * 2);
      expect(useGameStore.getState().gameState).toBeNull();
    } finally {
      watchdog.stop();
    }
    await resyncFromAdapter("test: empty store");
    expect(useGameStore.getState().gameState).toBeNull();
  });
});
