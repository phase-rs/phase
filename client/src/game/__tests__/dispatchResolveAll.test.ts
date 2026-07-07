import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { BatchResolveResult, GameState } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
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

function chunk(itemsResolved: number, total: number): BatchResolveResult {
  return { events: [], waitingFor: priorityWf, logEntries: [], itemsResolved, total };
}

describe("dispatchResolveAll progress", () => {
  let progressCalls: ({ resolved: number; total: number } | null)[];

  beforeEach(() => {
    progressCalls = [];
    usePreferencesStore.setState({ animationSpeedMultiplier: 1.0 });
    // Stack length read at each iteration start to classify pressure; keep it
    // in the "Instant" band (>=100) so the rAF-yield branch is exercised.
    useGameStore.setState({
      gameState: stateWithStack(200),
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

  it("latches the first chunk's total, accumulates + clamps the numerator, and clears at the end", async () => {
    // Per-chunk `total` SHRINKS (engine reports remaining stack); the latch must
    // keep the first chunk's 200. itemsResolved sums 80+80+80=240 > 200 → clamp.
    const resolveAll = vi
      .fn<EngineResolveAll>()
      .mockResolvedValueOnce(chunk(80, 200))
      .mockResolvedValueOnce(chunk(80, 150))
      .mockResolvedValueOnce(chunk(80, 100));

    // getState reports the board after each chunk; the 3rd empties the stack → done.
    const getState = vi
      .fn<() => Promise<GameState>>()
      .mockResolvedValueOnce(stateWithStack(200))
      .mockResolvedValueOnce(stateWithStack(200))
      .mockResolvedValueOnce(stateWithStack(0));

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
      } as never,
    });

    // Non-empty AI seat list = the "ai"-mode shape; an empty list would route
    // to the SetAutoPass fallback instead of the batch drain under test.
    await dispatchResolveAll(0, [{ playerId: 1, difficulty: "Medium" }]);

    // Three progress updates: total latched at 200 throughout; resolved
    // accumulates 80 -> 160 -> clamped 200.
    expect(progressCalls.slice(0, 3)).toEqual([
      { resolved: 80, total: 200 },
      { resolved: 160, total: 200 },
      { resolved: 200, total: 200 }, // min(240, 200) clamp
    ]);
    // Final call clears progress.
    expect(progressCalls[progressCalls.length - 1]).toBeNull();
    expect(useGameStore.getState().resolutionProgress).toBeNull();
    expect(useGameStore.getState().isResolvingAll).toBe(false);

    // rAF yield fired between the instant chunks (the load-bearing repaint fix):
    // 2 yields between 3 chunks.
    expect(rafSpy).toHaveBeenCalledTimes(2);
  });

  it("uses responsive instant chunks for giant stacks and marks Resolve All busy", async () => {
    useGameStore.setState({ gameState: stateWithStack(19192) });

    const resolveAll = vi.fn<EngineResolveAll>(async (_requester, _aiSeats, maxResolutions) => {
      expect(useGameStore.getState().isResolvingAll).toBe(true);
      expect(maxResolutions).toBe(5_000);
      return chunk(0, 19192);
    });

    useGameStore.setState({
      adapter: {
        resolveAll,
        getState: vi.fn().mockResolvedValue(stateWithStack(0)),
        getLegalActions: vi.fn().mockResolvedValue({ actions: [], autoPassRecommended: false }),
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

    useGameStore.setState({
      gameState: stateWithStack(3),
      adapter: {
        resolveAll,
        submitAction,
        getState: vi.fn().mockResolvedValue(stateWithStack(2)),
        getLegalActions: vi.fn().mockResolvedValue({ actions: [], autoPassRecommended: false }),
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

  it("falls back to an engine-side UntilStackEmpty auto-pass when the adapter has no batch resolveAll (multiplayer)", async () => {
    const submitAction = vi
      .fn<(action: unknown, actor: number) => Promise<{ events: never[] }>>()
      .mockResolvedValue({ events: [] });

    useGameStore.setState({
      gameState: stateWithStack(3),
      adapter: {
        submitAction,
        getState: vi.fn().mockResolvedValue(stateWithStack(2)),
        getLegalActions: vi.fn().mockResolvedValue({ actions: [], autoPassRecommended: false }),
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
