import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { BatchResolveResult, EngineSnapshot, GameState } from "../../adapter/types";
import { AdapterError, AdapterErrorCode, nextSnapshotSeq } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { useAppNotificationStore } from "../../stores/appToastStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { buildGameState, buildPriorityWaitingFor, buildStackEntry } from "../../test/factories/gameStateFactory";
import { dispatchResolveAll } from "../dispatch";

// A Priority-on-the-storming-player WaitingFor (active player holds priority).
const priorityWf: BatchResolveResult["waitingFor"] = buildPriorityWaitingFor();

function stateWithStack(len: number): GameState {
  return buildGameState({
    waiting_for: priorityWf,
    stack: Array.from({ length: len }, (_, index) => buildStackEntry({ id: index + 1 })),
  });
}

function readyStateWithStack(len: number): GameState {
  return buildGameState({
    waiting_for: { type: "ResolveAllReady", data: { epoch: 1 } },
    stack: Array.from({ length: len }, (_, index) => buildStackEntry({ id: index + 1 })),
  });
}

function chunk(itemsResolved: number, total: number): BatchResolveResult {
  return { events: [], waitingFor: priorityWf, logEntries: [], itemsResolved, total };
}

/**
 * A `getSnapshot` stub that reads through the test's own `getState` script and
 * pairs it with empty legal actions. The drain now reads ONE atomic pair per
 * chunk (not getState + getLegalActions), so this keeps each test's per-chunk
 * `getState` sequencing intact while matching the real adapter contract.
 */
function snapshotVia(getState: () => Promise<GameState>) {
  return vi.fn(async (): Promise<EngineSnapshot> => ({
    state: await getState(),
    legalResult: { actions: [], autoPassRecommended: false },
    seq: nextSnapshotSeq(),
  }));
}

describe("dispatchResolveAll progress", () => {
  let progressCalls: ({ resolved: number; total: number } | null)[];

  beforeEach(() => {
    progressCalls = [];
    usePreferencesStore.setState({ animationSpeedMultiplier: 1.0 });
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
    // Keep the stack in the Instant pressure band so the ready consumer uses
    // the engine's larger bounded-prefix cap.
    useGameStore.setState({
      gameState: readyStateWithStack(200),
      resolutionProgress: null,
      isResolvingAll: false,
      // Capture every setResolutionProgress call for assertions.
      setResolutionProgress: (p) => {
        progressCalls.push(p);
        useGameStore.setState({ resolutionProgress: p });
      },
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("reports the engine-proved prefix once and clears progress at the end", async () => {
    const resolveAll = vi.fn<EngineResolveAll>().mockResolvedValueOnce(chunk(80, 200));

    // The engine resolves the entire proved prefix in one bounded call, then
    // supplies one authoritative post-prefix snapshot.
    const getState = vi.fn<() => Promise<GameState>>().mockResolvedValueOnce(stateWithStack(120));

    const rafSpy = vi
      .spyOn(globalThis, "requestAnimationFrame")
      .mockImplementation((cb: FrameRequestCallback) => {
        cb(0);
        return 0;
      });

    useGameStore.setState({
      adapter: {
        resolveAll,
        getState,
        getLegalActions: vi.fn().mockResolvedValue({ actions: [], autoPassRecommended: false }),
        getSnapshot: snapshotVia(getState),
      } as never,
    });

    // Non-empty AI seat list = the "ai"-mode shape; an empty list would route
    // to the SetAutoPass fallback instead of the batch drain under test.
    await dispatchResolveAll(0, [{ playerId: 1, difficulty: "Medium" }]);

    expect(resolveAll).toHaveBeenCalledTimes(1);
    expect(progressCalls).toEqual([{ resolved: 80, total: 200 }, null]);
    // Final call clears progress.
    expect(progressCalls[progressCalls.length - 1]).toBeNull();
    expect(useGameStore.getState().resolutionProgress).toBeNull();
    expect(useGameStore.getState().isResolvingAll).toBe(false);

    expect(rafSpy).not.toHaveBeenCalled();
  });

  it("uses responsive instant chunks for giant stacks and marks Resolve All busy", async () => {
    useGameStore.setState({ gameState: readyStateWithStack(19192) });

    const resolveAll = vi.fn<EngineResolveAll>(async (_requester, _aiSeats, maxResolutions) => {
      expect(useGameStore.getState().isResolvingAll).toBe(true);
      expect(maxResolutions).toBe(5_000);
      return chunk(0, 19192);
    });

    const getState = vi.fn().mockResolvedValue(stateWithStack(0));
    useGameStore.setState({
      adapter: {
        resolveAll,
        getState,
        getLegalActions: vi.fn().mockResolvedValue({ actions: [], autoPassRecommended: false }),
        getSnapshot: snapshotVia(getState),
      } as never,
    });

    await dispatchResolveAll(0, [{ playerId: 1, difficulty: "Medium" }]);

    expect(resolveAll).toHaveBeenCalledTimes(1);
    expect(useGameStore.getState().isResolvingAll).toBe(false);
  });

  it("falls back to the auto-yield when there are no AI seats to drive the drain, even with a batch-capable adapter (local hotseat, #4978)", async () => {
    const resolveAll = vi.fn<EngineResolveAll>();
    const submitAction = vi
      .fn<(action: unknown, actor: number) => Promise<{ events: never[] }>>()
      .mockResolvedValue({ events: [] });

    const getState = vi.fn().mockResolvedValue(stateWithStack(2));
    useGameStore.setState({
      gameState: stateWithStack(3),
      adapter: {
        resolveAll,
        submitAction,
        getState,
        getLegalActions: vi.fn().mockResolvedValue({ actions: [], autoPassRecommended: false }),
        getSnapshot: snapshotVia(getState),
      } as never,
    });

    await dispatchResolveAll(0, []);

    // The batch drain needs an AI decider for every non-requester seat; with
    // none, those seats are humans (local hotseat) and CR 117.4 entitles each
    // to their own priority window — never engage the worker drain.
    expect(resolveAll).not.toHaveBeenCalled();
    expect(submitAction).toHaveBeenCalledWith(
      { type: "SetAutoPass", data: { mode: { type: "UntilStackEmpty" } } },
      0,
    );
  });

  it("consumes Ready consent before considering the empty-AI fallback", async () => {
    const resolveAll = vi.fn<EngineResolveAll>().mockResolvedValue(chunk(1, 2));
    const submitAction = vi.fn();
    const getState = vi.fn().mockResolvedValue(stateWithStack(1));
    useGameStore.setState({
      gameState: readyStateWithStack(2),
      adapter: {
        resolveAll,
        submitAction,
        getState,
        getLegalActions: vi.fn().mockResolvedValue({ actions: [], autoPassRecommended: false }),
        getSnapshot: snapshotVia(getState),
      } as never,
    });

    await dispatchResolveAll(0, []);

    expect(resolveAll).toHaveBeenCalledWith(0, [], 5);
    expect(submitAction).not.toHaveBeenCalled();
  });

  it("does not retain AI seats across the consent and Ready calls", async () => {
    const seats = [{ playerId: 1, difficulty: "Medium" }];
    const submitAction = vi.fn().mockResolvedValue({ events: [] });
    const consent = buildGameState({
      waiting_for: { type: "ResolveAllConsent", data: { epoch: 1, representative: 1 } },
      stack: Array.from({ length: 2 }, (_, index) => buildStackEntry({ id: index + 1 })),
    });
    useGameStore.setState({
      gameState: stateWithStack(2),
      adapter: {
        resolveAll: vi.fn<EngineResolveAll>(),
        submitAction,
        getSnapshot: vi.fn(async () => ({
          state: consent,
          legalResult: { actions: [], autoPassRecommended: false },
          seq: nextSnapshotSeq(),
        })),
      } as never,
    });

    await dispatchResolveAll(0, seats);

    expect(submitAction).toHaveBeenCalledWith(
      { type: "BeginResolveAll", data: { max_resolutions: 5 } },
      0,
    );

    const resolveAll = vi.fn<EngineResolveAll>().mockResolvedValue(chunk(1, 2));
    const getState = vi.fn().mockResolvedValue(stateWithStack(1));
    useGameStore.setState({
      gameState: readyStateWithStack(2),
      adapter: {
        resolveAll,
        getState,
        getLegalActions: vi.fn().mockResolvedValue({ actions: [], autoPassRecommended: false }),
        getSnapshot: snapshotVia(getState),
      } as never,
    });

    await dispatchResolveAll(0, []);

    expect(resolveAll).toHaveBeenCalledWith(0, [], 5);
  });
  it("consumes Ready consent with an empty AI-seat list when the server owns native AI", async () => {
    const resolveAll = vi.fn<EngineResolveAll>().mockResolvedValue(chunk(0, 2));
    const getState = vi.fn().mockResolvedValue(stateWithStack(0));
    const submitAction = vi.fn().mockResolvedValue({ events: [] });
    useGameStore.setState({
      gameState: readyStateWithStack(2),
      adapter: {
        resolveAll,
        resolveAllUsesServerAi: true,
        submitAction,
        getState,
        getLegalActions: vi.fn().mockResolvedValue({ actions: [], autoPassRecommended: false }),
        getSnapshot: snapshotVia(getState),
      } as never,
    });

    await dispatchResolveAll(0, []);

    expect(resolveAll).toHaveBeenCalledWith(0, [], 5);
    expect(submitAction).not.toHaveBeenCalled();
  });

  it("silently absorbs a stale Resolve All priority rejection without rejecting the click handler", async () => {
    const resolveAll = vi
      .fn<EngineResolveAll>()
      .mockRejectedValue(
        new AdapterError(
          AdapterErrorCode.STALE_ACTION,
          "Resolve All requires your priority",
          false,
        ),
      );
    const getState = vi.fn().mockResolvedValue(stateWithStack(2));
    useGameStore.setState({
      gameState: readyStateWithStack(2),
      adapter: {
        resolveAll,
        resolveAllUsesServerAi: true,
        getState,
        getLegalActions: vi.fn().mockResolvedValue({ actions: [], autoPassRecommended: false }),
        getSnapshot: snapshotVia(getState),
      } as never,
    });

    await expect(dispatchResolveAll(0, [])).resolves.toBeUndefined();

    expect(useAppNotificationStore.getState().notification).toBeNull();
    expect(useGameStore.getState().isResolvingAll).toBe(false);
    expect(useGameStore.getState().resolutionProgress).toBeNull();
  });

  it("still surfaces a non-stale Resolve All rejection", async () => {
    const resolveAll = vi
      .fn<EngineResolveAll>()
      .mockRejectedValue(new Error("batch snapshot rejected"));
    const getState = vi.fn().mockResolvedValue(stateWithStack(2));
    useGameStore.setState({
      gameState: readyStateWithStack(2),
      adapter: {
        resolveAll,
        resolveAllUsesServerAi: true,
        getState,
        getLegalActions: vi.fn().mockResolvedValue({ actions: [], autoPassRecommended: false }),
        getSnapshot: snapshotVia(getState),
      } as never,
    });

    await expect(dispatchResolveAll(0, [])).resolves.toBeUndefined();

    expect(useAppNotificationStore.getState().notification).toMatchObject({
      description: "batch snapshot rejected",
    });
  });

  it("falls back to an engine-side UntilStackEmpty auto-pass when the adapter has no batch resolveAll (multiplayer)", async () => {
    const submitAction = vi
      .fn<(action: unknown, actor: number) => Promise<{ events: never[] }>>()
      .mockResolvedValue({ events: [] });

    const getState = vi.fn().mockResolvedValue(stateWithStack(2));
    useGameStore.setState({
      gameState: stateWithStack(3),
      adapter: {
        submitAction,
        getState,
        getLegalActions: vi.fn().mockResolvedValue({ actions: [], autoPassRecommended: false }),
        getSnapshot: snapshotVia(getState),
      } as never,
    });

    // A NON-empty seat list pins the `!adapter.resolveAll` half of the
    // fallback gate on its own: even when a caller claims AI seats exist
    // (draft-match vs a human would, if its pairing were misread), a
    // transport with no batch drain must still take the auto-yield path.
    await dispatchResolveAll(0, [{ playerId: 1, difficulty: "Medium" }]);

    // Arena semantics: yield THIS seat's priority windows via the engine's
    // auto-pass session — never a host-driven batch drain over human seats.
    expect(submitAction).toHaveBeenCalledTimes(1);
    expect(submitAction).toHaveBeenCalledWith(
      { type: "SetAutoPass", data: { mode: { type: "UntilStackEmpty" } } },
      0,
    );
    // The batch busy-state must stay untouched — there is no local drain loop.
    expect(useGameStore.getState().isResolvingAll).toBe(false);
  });
});

type EngineResolveAll = (
  requester: number,
  aiSeats: { playerId: number; difficulty: string }[],
  maxResolutions?: number,
) => Promise<BatchResolveResult>;
