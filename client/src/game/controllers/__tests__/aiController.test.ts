import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  AdapterError,
  AdapterErrorCode,
  type AiActionProposal,
  type GameAction,
  type GameState,
  type WaitingFor,
} from "../../../adapter/types";
import { buildGameState } from "../../../test/factories/gameStateFactory";

const dispatchMocks = vi.hoisted(() => ({
  dispatchAiActionProposal: vi.fn<
    (proposal: AiActionProposal) => Promise<{ status: "applied" | "stale" }>
  >(),
}));
const { dispatchAiActionProposal } = dispatchMocks;
const notifyEngineLost = vi.fn();
const attemptStateRehydrate = vi.fn(async () => false);
const isEnginePanic = vi.fn<(error: unknown) => boolean>(() => false);
const routePanic = vi.fn<(reason: string, panic?: string) => Promise<void>>(async () => {});

vi.mock("../../dispatch", () => ({ dispatchAiActionProposal: dispatchMocks.dispatchAiActionProposal }));
vi.mock("../../engineRecovery", () => ({
  attemptStateRehydrate: () => attemptStateRehydrate(),
  isEnginePanic: (error: unknown) => isEnginePanic(error),
  notifyEngineLost: (...args: unknown[]) => notifyEngineLost(...args),
  routePanic: (reason: string, panic?: string) => routePanic(reason, panic),
}));
vi.mock("../../debugLog", () => ({ debugLog: vi.fn() }));

let storeState: {
  gameState: GameState | null;
  waitingFor: WaitingFor | null;
  adapter: { getAiActionProposal?: (difficulty: string, playerId: number) => Promise<AiActionProposal | null> } | null;
  gameSessionGeneration: number;
  isResolvingAll: boolean;
};
let storeSubscriber: (() => void) | null = null;
let randomSpy: ReturnType<typeof vi.spyOn>;

vi.mock("../../../stores/gameStore", () => ({
  useGameStore: {
    getState: () => storeState,
    subscribe: (_selector: unknown, callback: () => void) => {
      storeSubscriber = callback;
      return () => {
        if (storeSubscriber === callback) storeSubscriber = null;
      };
    },
  },
}));

import { createAIController } from "../aiController";

const PASS = { type: "PassPriority" } as GameAction;

function priorityState(): GameState {
  const waitingFor = { type: "Priority", data: { player: 1 } } as WaitingFor;
  return buildGameState({ waiting_for: waitingFor, priority_player: 1, stack: [] });
}

function proposal(action: GameAction): AiActionProposal {
  return { token: "engine-bound", semanticOwner: 1, actor: 1, action };
}

async function runOnce(): Promise<void> {
  await vi.advanceTimersByTimeAsync(1_000);
  await Promise.resolve();
  await Promise.resolve();
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.useFakeTimers();
  // Keep retry timing deterministic: `runOnce` advances a fixed 1s window,
  // while production delay variance would otherwise allow a variable number
  // of nested retries to fit inside that same window.
  randomSpy = vi.spyOn(Math, "random").mockReturnValue(1);
  dispatchAiActionProposal.mockReset();
  notifyEngineLost.mockReset();
  attemptStateRehydrate.mockReset();
  attemptStateRehydrate.mockResolvedValue(false);
  isEnginePanic.mockReset();
  isEnginePanic.mockReturnValue(false);
  routePanic.mockReset();
  const state = priorityState();
  storeState = {
    gameState: state,
    waitingFor: state.waiting_for,
    adapter: null,
    gameSessionGeneration: 1,
    isResolvingAll: false,
  };
});

afterEach(() => {
  storeSubscriber = null;
  randomSpy.mockRestore();
  vi.useRealTimers();
});

describe("AI proposal controller", () => {
  it("submits an engine-issued proposal exactly once without reconstructing its action", async () => {
    const issued = proposal(PASS);
    const getAiActionProposal = vi.fn(async () => issued);
    dispatchAiActionProposal.mockResolvedValue({ status: "applied" });
    storeState.adapter = { getAiActionProposal };

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    await runOnce();

    expect(getAiActionProposal).toHaveBeenCalledWith("Medium", 1);
    expect(dispatchAiActionProposal).toHaveBeenCalledTimes(1);
    expect(dispatchAiActionProposal).toHaveBeenCalledWith(issued);
    controller.dispose();
  });

  it("re-queries after the dispatch layer returns the engine's tagged stale outcome without fabricating an action", async () => {
    const issued = proposal(PASS);
    const getAiActionProposal = vi
      .fn<(difficulty: string, playerId: number) => Promise<AiActionProposal | null>>()
      .mockResolvedValueOnce(issued)
      .mockResolvedValue(null);
    dispatchAiActionProposal.mockResolvedValue({ status: "stale" });
    storeState.adapter = { getAiActionProposal };

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    await runOnce();
    await runOnce();

    expect(getAiActionProposal).toHaveBeenCalledWith("Medium", 1);
    expect(getAiActionProposal.mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(dispatchAiActionProposal).toHaveBeenCalledTimes(1);
    expect(notifyEngineLost).not.toHaveBeenCalled();
    expect(dispatchAiActionProposal).toHaveBeenCalledWith(issued);
    controller.dispose();
  });

  it("halts after rejected proposals without dispatching fabricated pass or cancel actions", async () => {
    const issued = proposal({ type: "ActivateAbility", data: { source_id: 44, ability_index: 0 } } as GameAction);
    const getAiActionProposal = vi.fn(async () => issued);
    dispatchAiActionProposal.mockRejectedValue(new Error("proposal rejected"));
    storeState.adapter = { getAiActionProposal };

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    await runOnce();
    await runOnce();
    await runOnce();
    await runOnce();

    expect(notifyEngineLost).toHaveBeenCalledWith("ai-controller-stuck:Priority");
    expect(dispatchAiActionProposal.mock.calls.every(([sent]) => sent.action.type === "ActivateAbility")).toBe(true);
    controller.dispose();
  });

  it("transports a targeted planeswalker activation as the engine-issued proposal", async () => {
    const loyalty = proposal({
      type: "ActivateAbility",
      data: { source_id: 99, ability_index: 1, targets: [7] },
    } as GameAction);
    const getAiActionProposal = vi.fn(async () => loyalty);
    dispatchAiActionProposal.mockResolvedValue({ status: "applied" });
    storeState.adapter = { getAiActionProposal };

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Hard" }] });
    controller.start();
    await runOnce();

    expect(dispatchAiActionProposal).toHaveBeenCalledWith(loyalty);
    controller.dispose();
  });

  it("rehydrates a lost engine state, then retries the same engine proposal lookup", async () => {
    const issued = proposal(PASS);
    const stateLost = new AdapterError(
      AdapterErrorCode.STATE_LOST,
      "engine state lost",
      true,
    );
    const getAiActionProposal = vi
      .fn<(difficulty: string, playerId: number) => Promise<AiActionProposal | null>>()
      .mockRejectedValueOnce(stateLost)
      .mockResolvedValueOnce(issued);
    attemptStateRehydrate.mockResolvedValue(true);
    dispatchAiActionProposal.mockResolvedValue({ status: "applied" });
    storeState.adapter = { getAiActionProposal };

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    await runOnce();

    expect(attemptStateRehydrate).toHaveBeenCalledOnce();
    expect(getAiActionProposal).toHaveBeenCalledTimes(3);
    expect(dispatchAiActionProposal).toHaveBeenCalledWith(issued);
    expect(notifyEngineLost).not.toHaveBeenCalled();
    controller.dispose();
  });

  it("drops a proposal computed before Resolve All takes ownership of Priority", async () => {
    const pendingProposal = deferred<AiActionProposal | null>();
    const getAiActionProposal = vi.fn(() => pendingProposal.promise);
    storeState.adapter = { getAiActionProposal };

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    vi.advanceTimersByTime(1_000);
    await Promise.resolve();

    storeState.isResolvingAll = true;
    storeSubscriber?.();
    pendingProposal.resolve(proposal(PASS));
    await Promise.resolve();
    await Promise.resolve();

    expect(dispatchAiActionProposal).not.toHaveBeenCalled();
    controller.dispose();
  });

  it("schedules Priority exactly once when Resolve All releases ownership", async () => {
    storeState.isResolvingAll = true;
    const issued = proposal(PASS);
    const getAiActionProposal = vi.fn(async () => issued);
    dispatchAiActionProposal.mockResolvedValue({ status: "applied" });
    storeState.adapter = { getAiActionProposal };

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    expect(getAiActionProposal).not.toHaveBeenCalled();

    storeState.isResolvingAll = false;
    storeSubscriber?.();
    await runOnce();

    expect(getAiActionProposal).toHaveBeenCalledTimes(2);
    expect(dispatchAiActionProposal).toHaveBeenCalledOnce();
    controller.dispose();
  });

  it("does not retry state recovery after a newer game session supersedes the attempt", async () => {
    const pendingFailure = deferred<AiActionProposal | null>();
    const getAiActionProposal = vi
      .fn<() => Promise<AiActionProposal | null>>()
      .mockReturnValueOnce(pendingFailure.promise)
      .mockResolvedValueOnce(null);
    storeState.adapter = { getAiActionProposal };

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    vi.advanceTimersByTime(1_000);
    await Promise.resolve();

    storeState.gameSessionGeneration += 1;
    storeSubscriber?.();
    pendingFailure.reject(new AdapterError(AdapterErrorCode.STATE_LOST, "engine state lost", true));
    await Promise.resolve();
    await Promise.resolve();

    expect(attemptStateRehydrate).not.toHaveBeenCalled();
    expect(notifyEngineLost).not.toHaveBeenCalled();
    expect(dispatchAiActionProposal).not.toHaveBeenCalled();
    controller.dispose();
  });
});
