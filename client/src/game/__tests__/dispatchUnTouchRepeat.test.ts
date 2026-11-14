/**
 * An Un-card touch (Vile Bile) is an event, not an answer to a prompt: each
 * pointer entry is its own instance. The dispatch queue's duplicate guard —
 * same action, same actor, same prompt — must not collapse repeated touches of
 * one object made while an earlier dispatch is still in flight.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

import type {
  EngineAdapter,
  EngineSnapshot,
  GameAction,
  SubmitResult,
} from "../../adapter/types";
import { nextSnapshotSeq } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import {
  buildGameState,
  buildLegalActionsResult,
  buildPriorityWaitingFor,
} from "../../test/factories/gameStateFactory";
import { dispatchAction } from "../dispatch";

const TOUCH: GameAction = {
  type: "UnAction",
  data: { action: { type: "TouchObject", data: { object_id: 7 } } },
};

function seed(submitAction: EngineAdapter["submitAction"]): EngineAdapter {
  const state = buildGameState({ waiting_for: buildPriorityWaitingFor() });
  const legal = buildLegalActionsResult({ actions: [{ type: "PassPriority" }] });
  const adapter: EngineAdapter = {
    initialize: vi.fn().mockResolvedValue(undefined),
    initializeGame: vi.fn().mockResolvedValue({ events: [] } as SubmitResult),
    restoreState: vi.fn(),
    estimateBracket: vi.fn().mockResolvedValue(null),
    dispose: vi.fn(),
    submitAction,
    getState: vi.fn(async () => state),
    getLegalActions: vi.fn(async () => legal),
    getSnapshot: vi.fn(async (): Promise<EngineSnapshot> => ({
      state,
      legalResult: legal,
      seq: nextSnapshotSeq(),
    })),
  };
  useGameStore.setState({
    gameId: null,
    gameMode: "ai",
    adapter,
    gameState: state,
    waitingFor: state.waiting_for,
    legalActions: legal.actions,
    events: [],
    eventHistory: [],
    logHistory: [],
    nextLogSeq: 0,
    stateHistory: [],
    turnCheckpoints: [],
    lastCommittedSeq: 0,
  });
  return adapter;
}

describe("dispatching Un-card touches", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
    vi.useFakeTimers();
  });

  it("submits every touch of the same object made while one is in flight", async () => {
    const adapter = seed(vi.fn(async (): Promise<SubmitResult> => ({ events: [], log_entries: [] })));

    const first = dispatchAction(TOUCH, 0);
    const second = dispatchAction(TOUCH, 0);
    const third = dispatchAction(TOUCH, 0);
    await vi.runAllTimersAsync();
    await Promise.all([first, second, third]);

    expect(adapter.submitAction).toHaveBeenCalledTimes(3);
    expect(vi.mocked(adapter.submitAction).mock.calls.every(([action]) => action === TOUCH)).toBe(true);
  });

  it("still drops a repeated prompt answer as a duplicate", async () => {
    const adapter = seed(vi.fn(async (): Promise<SubmitResult> => ({ events: [], log_entries: [] })));
    const pass: GameAction = { type: "PassPriority" };

    const first = dispatchAction(pass, 0);
    const second = dispatchAction(pass, 0);
    await vi.runAllTimersAsync();
    await Promise.all([first, second]);

    expect(adapter.submitAction).toHaveBeenCalledTimes(1);
  });
});
