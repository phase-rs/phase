import { beforeEach, describe, expect, it, vi } from "vitest";

import type {
  EngineAdapter,
  EngineSnapshot,
  FormatConfig,
  GameState,
  LegalActionsResult,
} from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory";
import {
  buildGameState,
  buildLegalActionsResult,
  buildPriorityWaitingFor,
} from "../../test/factories/gameStateFactory";
import { attemptStateRehydrate } from "../engineRecovery";

vi.mock("idb-keyval", () => ({
  createStore: vi.fn(() => ({})),
  del: vi.fn().mockResolvedValue(undefined),
  get: vi.fn().mockResolvedValue(undefined),
  set: vi.fn().mockResolvedValue(undefined),
}));

import { get as idbGet } from "idb-keyval";

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
    vi.clearAllMocks();
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
      dispose: vi.fn(),
    } as unknown as EngineAdapter;
    const newAdapter = { dispose: vi.fn() } as unknown as EngineAdapter;

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

  it("migrates a pre-v42 checkpoint before the real restore path", async () => {
    const checkpoint = buildGameState({ waiting_for: PRIORITY, turn_number: 4 });
    checkpoint.format_config = {
      ...checkpoint.format_config,
      format: "CommanderDraft",
      command_zone: true,
      deck_size: 60 as never,
    } as FormatConfig;
    vi.mocked(idbGet).mockResolvedValueOnce([checkpoint]);

    const restored: GameState[] = [];
    const adapter = buildEngineAdapterMock(checkpoint, {
      restoreState: vi.fn(async (state: GameState) => {
        restored.push(state);
      }),
    });
    useGameStore.setState({
      adapter,
      gameMode: "local",
      gameState: null,
      gameId: "legacy-checkpoint",
      gameSessionGeneration: 1,
    });

    await expect(attemptStateRehydrate()).resolves.toBe(true);
    expect(restored).toHaveLength(1);
    expect(restored[0]).toMatchObject({
      format_config: { deck_size: { type: "Minimum", data: 60 } },
    });
  });
});
