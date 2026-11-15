import { afterEach, describe, expect, it, vi } from "vitest";

import type { EngineSnapshot, GameState } from "../../adapter/types";
import { nextSnapshotSeq } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { buildGameState, buildPriorityWaitingFor, buildStackEntry } from "../../test/factories/gameStateFactory";
import { dispatchResolveAll } from "../dispatch";

function stateWithStack(): GameState {
  return buildGameState({
    waiting_for: buildPriorityWaitingFor(),
    stack: [buildStackEntry({ id: 1 })],
  });
}

describe("dispatchResolveAll", () => {
  afterEach(() => vi.restoreAllMocks());

  it("only begins the engine-owned consent transaction", async () => {
    const state = stateWithStack();
    const submitAction = vi.fn().mockResolvedValue({ events: [], log_entries: [] });
    const getSnapshot = vi.fn<() => Promise<EngineSnapshot>>().mockResolvedValue({
      state,
      legalResult: { actions: [], autoPassRecommended: false },
      seq: nextSnapshotSeq(),
    });
    const resolveAll = vi.fn();
    useGameStore.setState({ gameState: state, waitingFor: state.waiting_for, adapter: {
      submitAction,
      getSnapshot,
      resolveAll,
    } as never });

    await dispatchResolveAll(0);

    expect(submitAction).toHaveBeenCalledWith(
      { type: "BeginResolveAll", data: { max_resolutions: 0, scope: { type: "Own" } } },
      0,
    );
    expect(resolveAll).not.toHaveBeenCalled();
  });
});
