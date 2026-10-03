import "../../../test/helpers/persistedStorage";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ActionResult, AiActionProposal, EngineSnapshot, GameState } from "../../../adapter/types";
import { nextSnapshotSeq } from "../../../adapter/types";
import { useGameStore } from "../../../stores/gameStore";
import { buildEngineAdapterMock } from "../../../test/factories/engineAdapterFactory";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory";
import { buildLegalActionsResult, gameStateFactory } from "../../../test/factories/gameStateFactory";
import { useUiStore } from "../../../stores/uiStore";
import { dispatchActionForGameSession, isDispatchIdle, processRemoteUpdate } from "../../dispatch";
import { notifyEngineLost } from "../../engineRecovery";
import { createAIController, type AIController } from "../aiController";
import { createGameLoopController } from "../gameLoopController";

vi.mock("../../engineRecovery", () => ({
  attemptStateRehydrate: async () => false,
  isEnginePanic: () => false,
  notifyEngineLost: vi.fn(),
  routePanic: async () => {},
}));
vi.mock("../../debugLog", () => ({ debugLog: vi.fn() }));

function priorityState(player: number): GameState {
  return gameStateFactory.withPlayers(0, 1, 2).activePlayer(player).priority(player).build();
}

function snapshotOf(state: GameState): EngineSnapshot {
  return { state, legalResult: buildLegalActionsResult(), seq: nextSnapshotSeq() };
}

function proposal(semanticOwner: number, actor = semanticOwner): AiActionProposal {
  return { token: "live-engine-proposal", semanticOwner, actor, action: { type: "PassPriority" } };
}

describe("AI priority snapshot handoff", () => {
  let controller: AIController;

  beforeEach(() => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(1);
    vi.mocked(notifyEngineLost).mockClear();
    useGameStore.getState().reset();
    controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
  });

  afterEach(() => {
    controller.dispose();
    useGameStore.getState().reset();
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it("commits live human priority through the real dispatch pipeline without submitting the mismatched proposal", async () => {
    const displayed = priorityState(1);
    const live = priorityState(0);
    const adapter = buildEngineAdapterMock(live, {
      getAiActionProposal: vi.fn().mockResolvedValue(proposal(0)),
    });
    await processRemoteUpdate(snapshotOf(displayed), []);
    useGameStore.setState({ adapter });
    controller.start();
    await vi.advanceTimersByTimeAsync(10_000);

    expect(adapter.getSnapshot).toHaveBeenCalledTimes(1);
    expect(useGameStore.getState().gameState).toBe(live);
    expect(useGameStore.getState().waitingFor).toEqual(live.waiting_for);
    expect(adapter.submitAiActionProposal).not.toHaveBeenCalled();
    expect(notifyEngineLost).not.toHaveBeenCalled();
    expect(adapter.getAiActionProposal).toHaveBeenCalledTimes(1);
  });

  it("halts after six unchanged mismatches, including tactical fallback attempts", async () => {
    const displayed = priorityState(1);
    const adapter = buildEngineAdapterMock(displayed, {
      getAiActionProposal: vi.fn().mockResolvedValue(proposal(0)),
      getAiTacticalActionProposal: vi.fn().mockResolvedValue(proposal(0)),
    });
    await processRemoteUpdate(snapshotOf(displayed), []);
    useGameStore.setState({ adapter });
    controller.start();
    await vi.advanceTimersByTimeAsync(10_000);

    expect(adapter.getAiActionProposal).toHaveBeenCalledTimes(3);
    expect(adapter.getAiTacticalActionProposal).toHaveBeenCalledTimes(3);
    expect(adapter.getSnapshot).toHaveBeenCalledTimes(6);
    expect(adapter.submitAiActionProposal).not.toHaveBeenCalled();
    expect(useGameStore.getState().gameState).toBe(displayed);
    expect(notifyEngineLost).toHaveBeenCalledExactlyOnceWith("ai-controller-stuck:Priority");
  });

  it("does not queue a reconciliation snapshot behind dispatch across game teardown", async () => {
    const displayed = priorityState(1);
    const live = priorityState(0);
    let finishSubmission!: (result: ActionResult) => void;
    const pendingSubmission = new Promise<ActionResult>((resolve) => { finishSubmission = resolve; });
    const adapter = buildEngineAdapterMock(live, {
      getAiActionProposal: vi.fn().mockResolvedValue(proposal(0)),
      submitAction: vi.fn().mockReturnValue(pendingSubmission),
    });
    await processRemoteUpdate(snapshotOf(displayed), []);
    useGameStore.setState({ adapter });
    const pendingDispatch = dispatchActionForGameSession(
      { type: "SetPhaseStops", data: { stops: [] } },
      adapter,
      useGameStore.getState().gameSessionGeneration,
      0,
    );
    expect(isDispatchIdle()).toBe(false);
    controller.start();
    await vi.advanceTimersByTimeAsync(10_000);
    const snapshotReads = vi.mocked(adapter.getSnapshot).mock.calls.length;
    expect(snapshotReads).toBeGreaterThan(6);
    expect(useGameStore.getState().gameState).toBe(displayed);
    expect(notifyEngineLost).not.toHaveBeenCalled();

    controller.dispose();
    useGameStore.getState().reset();
    finishSubmission({ events: [], waiting_for: live.waiting_for });
    await pendingDispatch;
    await vi.advanceTimersByTimeAsync(0);

    expect(isDispatchIdle()).toBe(true);
    expect(useGameStore.getState().gameState).toBeNull();
    expect(useGameStore.getState().waitingFor).toBeNull();
    expect(adapter.getSnapshot).toHaveBeenCalledTimes(snapshotReads);
    expect(adapter.submitAiActionProposal).not.toHaveBeenCalled();
    expect(notifyEngineLost).not.toHaveBeenCalled();
  });

  it("wakes the human auto-pass controller after reconciling priority", async () => {
    const displayed = priorityState(1);
    const live = gameStateFactory.withPlayers(0, 1).activePlayer(0).priority(0)
      .withObjects(gameObjectFactory.creature(2, 2).onBattlefield().build()).build();
    const adapter = buildEngineAdapterMock(live, {
      getAiActionProposal: vi.fn().mockResolvedValue(proposal(0)),
      getLegalActions: vi.fn()
        .mockResolvedValueOnce(buildLegalActionsResult({ autoPassRecommended: true }))
        .mockResolvedValue(buildLegalActionsResult()),
    });
    await processRemoteUpdate(snapshotOf(displayed), []);
    useGameStore.setState({ adapter });
    useUiStore.setState({ fullControl: false });
    controller = createGameLoopController({ mode: "ai", playerCount: 2 });
    controller.start();
    await vi.advanceTimersByTimeAsync(1_500);

    expect(useGameStore.getState().waitingFor).toEqual(live.waiting_for);
    expect(adapter.submitAction).toHaveBeenCalledExactlyOnceWith({ type: "PassPriority" }, 0);
    expect(adapter.submitAiActionProposal).not.toHaveBeenCalled();
    expect(notifyEngineLost).not.toHaveBeenCalled();
    expect(useGameStore.getState().autoPassRecommended).toBe(false);
  });

  it.each(["disposed", "new session", "adapter swapped"])(
    "drops a pending snapshot when the controller is %s",
    async (change) => {
      const displayed = priorityState(1);
      const live = priorityState(0);
      let resolveSnapshot!: (snapshot: EngineSnapshot) => void;
      const pendingSnapshot = new Promise<EngineSnapshot>((resolve) => { resolveSnapshot = resolve; });
      const adapter = buildEngineAdapterMock(live, {
        getAiActionProposal: vi.fn().mockResolvedValue(proposal(0)),
        getSnapshot: vi.fn().mockReturnValue(pendingSnapshot),
      });
      await processRemoteUpdate(snapshotOf(displayed), []);
      useGameStore.setState({ adapter });
      controller.start();
      await vi.advanceTimersByTimeAsync(1_000);
      expect(adapter.getSnapshot).toHaveBeenCalledTimes(1);

      if (change === "disposed") controller.dispose();
      if (change === "new session") {
        useGameStore.setState({ gameSessionGeneration: useGameStore.getState().gameSessionGeneration + 1 });
      }
      if (change === "adapter swapped") {
        useGameStore.setState({ adapter: buildEngineAdapterMock(displayed) });
      }
      resolveSnapshot(snapshotOf(live));
      await vi.advanceTimersByTimeAsync(0);

      expect(useGameStore.getState().gameState).toBe(displayed);
      expect(useGameStore.getState().waitingFor).toEqual(displayed.waiting_for);
      expect(adapter.submitAiActionProposal).not.toHaveBeenCalled();
      expect(notifyEngineLost).not.toHaveBeenCalled();
    },
  );

  it("submits controlled-turn proposals whose actor differs from their matching semantic owner", async () => {
    const displayed = gameStateFactory.withPlayers(0, 1, 2).activePlayer(2).priority(1).build();
    const issued = proposal(1, 2);
    const adapter = buildEngineAdapterMock(displayed, {
      getAiActionProposal: vi.fn().mockResolvedValueOnce(issued).mockResolvedValue(null),
    });
    await processRemoteUpdate(snapshotOf(displayed), []);
    useGameStore.setState({ adapter });
    controller.start();
    await vi.advanceTimersByTimeAsync(1_000);

    expect(adapter.submitAiActionProposal).toHaveBeenCalledExactlyOnceWith(issued);
    expect(adapter.getAiActionProposal).toHaveBeenCalledWith("Medium", 2);
    expect(adapter.getSnapshot).not.toHaveBeenCalled();
    expect(notifyEngineLost).not.toHaveBeenCalled();
  });
});
