import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { BatchResolveResult, GameState } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { dispatchResolveAll } from "../dispatch";

// A Priority-on-the-storming-player WaitingFor (active player holds priority).
const priorityWf = { type: "Priority", data: { player: 0 } } as unknown as BatchResolveResult["waitingFor"];

function stateWithStack(len: number): GameState {
  return { waiting_for: priorityWf, stack: new Array(len).fill(0), turn: { active_player: 0 } } as unknown as GameState;
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

    await dispatchResolveAll(0, []);

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

    await dispatchResolveAll(0, []);

    expect(resolveAll).toHaveBeenCalledTimes(1);
    expect(useGameStore.getState().isResolvingAll).toBe(false);
  });
});

type EngineResolveAll = (
  requester: number,
  aiSeats: { playerId: number; difficulty: string }[],
  maxResolutions?: number,
) => Promise<BatchResolveResult>;
