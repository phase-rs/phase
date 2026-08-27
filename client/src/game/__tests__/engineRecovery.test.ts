import { beforeEach, describe, expect, it, vi } from "vitest";

import type { EngineAdapter, EngineSnapshot, GameState, LegalActionsResult } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import {
  buildGameState,
  buildLegalActionsResult,
  buildPriorityWaitingFor,
} from "../../test/factories/gameStateFactory";
import { attemptStateRehydrate } from "../engineRecovery";

const PRIORITY = buildPriorityWaitingFor({ data: { player: 0 } });
const LEGAL = buildLegalActionsResult({ actions: [] }) as LegalActionsResult;

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

describe("engine recovery session fencing", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  it("drops a restored automation snapshot after the game session changes", async () => {
    const staleState = buildGameState({ waiting_for: PRIORITY, turn_number: 3 });
    const replacementState = buildGameState({ waiting_for: PRIORITY, turn_number: 8 });
    const resumed = deferred<{
      snapshot: EngineSnapshot;
      presentation: {
        outcome: "progressed";
        automatedResolutionCount: number;
        omittedEventCount: number;
        logEntries: [];
      };
    }>();
    const oldAdapter = {
      restoreState: vi.fn(async () => undefined),
      resumeRestoredGameState: vi.fn(() => resumed.promise),
    } as unknown as EngineAdapter;
    const newAdapter = {} as EngineAdapter;

    useGameStore.setState({
      adapter: oldAdapter,
      gameMode: "local",
      gameState: staleState,
      gameId: "old-game",
      gameSessionGeneration: 17,
      lastCommittedSeq: 20,
      restoredStackAutomation: null,
    });

    const recovery = attemptStateRehydrate();
    await Promise.resolve();
    await Promise.resolve();
    expect(oldAdapter.restoreState).toHaveBeenCalledWith(staleState);
    expect(oldAdapter.resumeRestoredGameState).toHaveBeenCalledOnce();

    useGameStore.setState({
      adapter: newAdapter,
      gameState: replacementState,
      gameSessionGeneration: 18,
      lastCommittedSeq: 30,
      restoredStackAutomation: null,
    });
    resumed.resolve({
      snapshot: {
        state: staleState,
        legalResult: LEGAL,
        seq: 99,
      },
      presentation: {
        outcome: "progressed",
        automatedResolutionCount: 1,
        omittedEventCount: 0,
        logEntries: [],
      },
    });

    await expect(recovery).resolves.toBe(true);
    expect(useGameStore.getState().adapter).toBe(newAdapter);
    expect(useGameStore.getState().gameState).toBe(replacementState);
    expect(useGameStore.getState().lastCommittedSeq).toBe(30);
    expect(useGameStore.getState().restoredStackAutomation).toBeNull();
  });
});
