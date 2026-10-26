import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  AdapterError,
  AdapterErrorCode,
  type AiActionProposal,
  type GameAction,
  type GameState,
  type WaitingFor,
} from "../../../adapter/types";
import { buildGameState, buildPendingCast } from "../../../test/factories/gameStateFactory";

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

vi.mock("../../dispatch", () => ({
  dispatchAiActionProposal: dispatchMocks.dispatchAiActionProposal,
}));
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
  adapter: {
    getAiActionProposal?: (difficulty: string, playerId: number) => Promise<AiActionProposal | null>;
    getAiTacticalActionProposal?: (difficulty: string, playerId: number) => Promise<AiActionProposal | null>;
  } | null;
  gameSessionGeneration: number;
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

  it("retries after a synchronous proposal lookup error instead of stranding the controller pending", async () => {
    const getAiActionProposal = vi.fn(() => {
      throw new Error("synchronous adapter failure");
    });
    storeState.adapter = { getAiActionProposal };

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    await runOnce();
    await runOnce();

    expect(getAiActionProposal.mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(dispatchAiActionProposal).not.toHaveBeenCalled();
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

  it("recovers rejected proposals with an engine-issued tactical fallback", async () => {
    const rejected = proposal({ type: "ActivateAbility", data: { source_id: 44, ability_index: 0 } } as GameAction);
    const fallback = proposal(PASS);
    const getAiActionProposal = vi.fn(async () => rejected);
    const getAiTacticalActionProposal = vi.fn(async () => fallback);
    dispatchAiActionProposal
      .mockRejectedValueOnce(new Error("proposal rejected"))
      .mockRejectedValueOnce(new Error("proposal rejected"))
      .mockRejectedValueOnce(new Error("proposal rejected"))
      .mockResolvedValue({ status: "applied" });
    storeState.adapter = { getAiActionProposal, getAiTacticalActionProposal };

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    await runOnce();
    await runOnce();
    await runOnce();
    await runOnce();

    expect(getAiActionProposal).toHaveBeenCalledTimes(3);
    expect(getAiTacticalActionProposal).toHaveBeenCalledWith("Medium", 1);
    expect(dispatchAiActionProposal.mock.calls.map(([sent]) => sent.action)).toEqual([
      rejected.action,
      rejected.action,
      rejected.action,
      fallback.action,
    ]);
    expect(notifyEngineLost).not.toHaveBeenCalled();
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

  it("leaves final Resolve All consent automation entirely to the engine", async () => {
    const consent = {
      type: "ResolveAllConsent",
      data: { epoch: 7, representative: 1 },
    } as WaitingFor;
    const ready = { type: "ResolveAllReady", data: { epoch: 7 } } as WaitingFor;
    const state = buildGameState({ waiting_for: consent, priority_player: 0, stack: [] });
    storeState.gameState = state;
    storeState.waitingFor = consent;
    const issued: AiActionProposal = {
      token: "engine-bound-consent",
      semanticOwner: 1,
      actor: 0,
      action: {
        type: "RespondResolveAllConsent",
        data: { epoch: 7, decision: { type: "Grant" } },
      },
    };
    const getAiActionProposal = vi.fn(async () => issued);
    storeState.adapter = { getAiActionProposal };
    dispatchAiActionProposal.mockImplementation(async () => {
      storeState.gameState = { ...state, waiting_for: ready };
      storeState.waitingFor = ready;
      storeSubscriber?.();
      return { status: "applied" };
    });

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    await runOnce();

    expect(getAiActionProposal).toHaveBeenCalledWith("Medium", 1);
    expect(dispatchAiActionProposal).toHaveBeenCalledWith(issued);
    controller.dispose();
  });

  it("does not start Resolve All after an AI representative declines consent", async () => {
    const consent = {
      type: "ResolveAllConsent",
      data: { epoch: 7, representative: 1 },
    } as WaitingFor;
    const state = buildGameState({ waiting_for: consent, priority_player: 0, stack: [] });
    const issued: AiActionProposal = {
      token: "engine-bound-consent-decline",
      semanticOwner: 1,
      actor: 0,
      action: {
        type: "RespondResolveAllConsent",
        data: { epoch: 7, decision: { type: "Decline" } },
      },
    };
    storeState.gameState = state;
    storeState.waitingFor = consent;
    storeState.adapter = { getAiActionProposal: vi.fn(async () => issued) };
    dispatchAiActionProposal.mockResolvedValue({ status: "applied" });

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    await runOnce();

    expect(dispatchAiActionProposal).toHaveBeenCalledWith(issued);
    controller.dispose();
  });

  it("does not act when the local human is the Resolve All consent representative", async () => {
    const consent = {
      type: "ResolveAllConsent",
      data: { epoch: 7, representative: 0 },
    } as WaitingFor;
    const state = buildGameState({ waiting_for: consent, priority_player: 1, stack: [] });
    const getAiActionProposal = vi.fn(async () => proposal(PASS));
    storeState.gameState = state;
    storeState.waitingFor = consent;
    storeState.adapter = { getAiActionProposal };

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    await runOnce();

    expect(getAiActionProposal).not.toHaveBeenCalled();
    expect(dispatchAiActionProposal).not.toHaveBeenCalled();
    controller.dispose();
  });

  it("does not start Resolve All when the Ready epoch differs from the submitted consent", async () => {
    const consent = {
      type: "ResolveAllConsent",
      data: { epoch: 7, representative: 1 },
    } as WaitingFor;
    const ready = { type: "ResolveAllReady", data: { epoch: 8 } } as WaitingFor;
    const state = buildGameState({ waiting_for: consent, priority_player: 0, stack: [] });
    storeState.gameState = state;
    storeState.waitingFor = consent;
    storeState.adapter = {
      getAiActionProposal: vi.fn<() => Promise<AiActionProposal>>(async () => ({
        token: "engine-bound-consent",
        semanticOwner: 1,
        actor: 0,
        action: {
          type: "RespondResolveAllConsent",
          data: { epoch: 7, decision: { type: "Grant" } },
        },
      })),
    };
    dispatchAiActionProposal.mockImplementation(async () => {
      storeState.gameState = { ...state, waiting_for: ready };
      storeState.waitingFor = ready;
      storeSubscriber?.();
      return { status: "applied" };
    });

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    await runOnce();

    controller.dispose();
  });

  it("does not start Resolve All for a stale consent submission even if Ready coincides", async () => {
    const consent = {
      type: "ResolveAllConsent",
      data: { epoch: 7, representative: 1 },
    } as WaitingFor;
    const ready = { type: "ResolveAllReady", data: { epoch: 7 } } as WaitingFor;
    const state = buildGameState({ waiting_for: consent, priority_player: 0, stack: [] });
    storeState.gameState = state;
    storeState.waitingFor = consent;
    storeState.adapter = {
      getAiActionProposal: vi.fn<() => Promise<AiActionProposal>>(async () => ({
        token: "engine-bound-consent",
        semanticOwner: 1,
        actor: 0,
        action: {
          type: "RespondResolveAllConsent",
          data: { epoch: 7, decision: { type: "Grant" } },
        },
      })),
    };
    dispatchAiActionProposal.mockImplementation(async () => {
      storeState.gameState = { ...state, waiting_for: ready };
      storeState.waitingFor = ready;
      storeSubscriber?.();
      return { status: "stale" };
    });

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    await runOnce();

    controller.dispose();
  });

  it("does not start Resolve All after a pending consent submission is superseded by a new session", async () => {
    const consent = {
      type: "ResolveAllConsent",
      data: { epoch: 7, representative: 1 },
    } as WaitingFor;
    const ready = { type: "ResolveAllReady", data: { epoch: 7 } } as WaitingFor;
    const state = buildGameState({ waiting_for: consent, priority_player: 0, stack: [] });
    const pendingSubmission = deferred<{ status: "applied" | "stale" }>();
    storeState.gameState = state;
    storeState.waitingFor = consent;
    storeState.adapter = {
      getAiActionProposal: vi.fn<() => Promise<AiActionProposal>>(async () => ({
        token: "engine-bound-consent",
        semanticOwner: 1,
        actor: 0,
        action: {
          type: "RespondResolveAllConsent",
          data: { epoch: 7, decision: { type: "Grant" } },
        },
      })),
    };
    dispatchAiActionProposal.mockReturnValue(pendingSubmission.promise);

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    await runOnce();

    storeState.gameSessionGeneration += 1;
    storeSubscriber?.();
    storeState.gameState = { ...state, waiting_for: ready };
    storeState.waitingFor = ready;
    storeSubscriber?.();
    pendingSubmission.resolve({ status: "applied" });
    await Promise.resolve();
    await Promise.resolve();

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

type FenceFields = { waiting_for_identity?: string; state_revision?: number };
const ENGINE_FENCE: FenceFields = { waiting_for_identity: "prompt-x", state_revision: 5 };

function paymentState(fields: FenceFields, printing = "SAAA", actor = 1): GameState {
  // Real wire carrier: PayCost.resume.Spell.ability.trigger_source.lki.printing.
  // ResolvedAbility's client interface intentionally only models fields its UI reads.
  const ability = {
    targets: [],
    trigger_source: { lki: { power: 2, printing: { set_code: printing, collector_number: "1" } } },
  };
  const waitingFor: WaitingFor = {
    type: "PayCost",
    data: {
      player: actor,
      kind: { type: "Discard" },
      choices: [10, 11, 12],
      count: 2,
      min_count: 2,
      resume: { type: "Spell", Spell: buildPendingCast({ ability }) },
    },
  };
  return buildGameState({ waiting_for: waitingFor, priority_player: actor, derived: fields, stack: [] });
}

function commitPrompt(state: GameState, notify: boolean): void {
  storeState.gameState = state;
  storeState.waitingFor = state.waiting_for;
  if (notify) storeSubscriber?.();
}

interface FenceCase {
  row: string;
  start?: FenceFields;
  next?: FenceFields;
  printing?: string;
  notify?: boolean;
  stale: boolean;
  session?: boolean;
  actor?: number;
  back?: boolean;
  sameReference?: boolean;
}

const fenceCases: FenceCase[] = [
  { row: "T1 engine display-only notification", printing: "SBBB", notify: true, stale: false },
  { row: "T2 identity notification", next: { ...ENGINE_FENCE, waiting_for_identity: "prompt-y" }, notify: true, stale: true },
  { row: "T2 identity completion without notification", next: { ...ENGINE_FENCE, waiting_for_identity: "prompt-y" }, stale: true },
  ...[6, 0].map((revision) => ({
    row: `T3 revision notification ${revision}`, next: { ...ENGINE_FENCE, state_revision: revision }, notify: true, stale: true,
  })),
  { row: "T4 identity changes back", next: { ...ENGINE_FENCE, waiting_for_identity: "prompt-y" }, notify: true, back: true, stale: true },
  { row: "T4 revision changes back", next: { ...ENGINE_FENCE, state_revision: 6 }, notify: true, back: true, stale: true },
  { row: "T5 session notification", session: true, notify: true, stale: true },
  { row: "T6 authorized actor notification", actor: 2, notify: true, stale: true },
  { row: "T6 authorized actor completion", actor: 2, stale: true },
  { row: "T6 authorized actor changes back", actor: 2, notify: true, back: true, stale: true },
  { row: "T7a display-only completion", printing: "SBBB", stale: false },
  ...[6, 0].map((revision) => ({
    row: `T7b revision completion ${revision}`, next: { ...ENGINE_FENCE, state_revision: revision }, stale: true,
  })),
  { row: "T8a legacy printing notification", start: {}, next: {}, printing: "SBBB", notify: true, stale: true },
  { row: "T8b legacy identical completion", start: {}, next: {}, stale: false },
  { row: "T8c legacy changed completion", start: {}, next: {}, printing: "SBBB", stale: true },
  ...[
    { waiting_for_identity: "prompt-x" },
    { state_revision: 5 },
  ].flatMap((fields, index) => [
    { row: `T9 incomplete ${index} notification`, start: fields, next: fields, notify: true, stale: true },
    { row: `T9 incomplete ${index} identical completion`, start: fields, next: fields, stale: false },
    { row: `T9 incomplete ${index} changed completion`, start: fields, next: fields, printing: "SBBB", stale: true },
  ]),
  { row: "T9 zero revision present", start: { ...ENGINE_FENCE, state_revision: 0 }, next: { ...ENGINE_FENCE, state_revision: 0 }, printing: "SBBB", notify: true, stale: false },
  { row: "T10 engine to legacy identical completion", next: {}, stale: false },
  { row: "T11 engine to legacy changed completion", next: {}, printing: "SBBB", stale: true },
  { row: "T12 legacy to engine identical completion", start: {}, stale: false },
  { row: "T13 legacy to engine changed completion", start: {}, printing: "SBBB", stale: true },
  ...[false, true].flatMap((toEngine) => [
    { row: `T14 mixed ${toEngine} identical notification`, start: toEngine ? {} : ENGINE_FENCE, next: toEngine ? ENGINE_FENCE : {}, notify: true, stale: true },
    { row: `T14 mixed ${toEngine} changed notification`, start: toEngine ? {} : ENGINE_FENCE, next: toEngine ? ENGINE_FENCE : {}, printing: "SBBB", notify: true, stale: true },
    { row: `T14 mixed ${toEngine} same-reference notification`, start: toEngine ? {} : ENGINE_FENCE, next: toEngine ? ENGINE_FENCE : {}, sameReference: true, notify: true, stale: false },
  ]),
];

describe("engine prompt fence and exact legacy fallback", () => {
  it.each(fenceCases)("$row", async (testCase) => {
    const first = paymentState(testCase.start ?? ENGINE_FENCE);
    commitPrompt(first, false);
    const pendingProposal = deferred<AiActionProposal | null>();
    const replacement = deferred<AiActionProposal | null>();
    const issued = proposal(PASS);
    const getAiActionProposal = vi.fn<(difficulty: string, playerId: number) => Promise<AiActionProposal | null>>()
      .mockReturnValueOnce(pendingProposal.promise)
      .mockReturnValue(replacement.promise);
    storeState.adapter = { getAiActionProposal };
    dispatchAiActionProposal.mockImplementation(async () => {
      storeState.gameState = null;
      storeState.waitingFor = null;
      return { status: "applied" };
    });
    const controller = createAIController({ seats: [
      { playerId: 1, difficulty: "Medium" },
      { playerId: 2, difficulty: "Medium" },
    ] });
    try {
      controller.start();
      await Promise.resolve();
      await Promise.resolve();
      expect(getAiActionProposal).toHaveBeenCalledExactlyOnceWith("Medium", 1);
      if (testCase.session) storeState.gameSessionGeneration += 1;
      const next = paymentState(testCase.next ?? ENGINE_FENCE, testCase.printing ?? "SAAA", testCase.actor ?? 1);
      if (testCase.sameReference) next.waiting_for = first.waiting_for;
      commitPrompt(next, testCase.notify ?? false);
      if (testCase.notify && testCase.stale) {
        // Isolate the subscription: replacement work must start while the original
        // proposal is still unresolved, before the completion guard can reject it.
        await Promise.resolve();
        await Promise.resolve();
        expect(getAiActionProposal).toHaveBeenCalledTimes(2);
        expect(getAiActionProposal).toHaveBeenLastCalledWith("Medium", testCase.actor ?? 1);
        expect(dispatchAiActionProposal).not.toHaveBeenCalled();
      }
      if (testCase.back) {
        commitPrompt(paymentState(ENGINE_FENCE), true);
        await Promise.resolve();
        await Promise.resolve();
        expect(getAiActionProposal).toHaveBeenCalledTimes(3);
        expect(getAiActionProposal).toHaveBeenLastCalledWith("Medium", 1);
      }
      pendingProposal.resolve(issued);
      await runOnce();
      if (testCase.stale) {
        expect(dispatchAiActionProposal).not.toHaveBeenCalled();
        expect(getAiActionProposal.mock.calls.length).toBeGreaterThanOrEqual(2);
        if (testCase.actor) expect(getAiActionProposal).toHaveBeenLastCalledWith("Medium", testCase.back ? 1 : testCase.actor);
      } else {
        expect(dispatchAiActionProposal).toHaveBeenCalledExactlyOnceWith(issued);
        expect(getAiActionProposal).toHaveBeenCalledTimes(1);
      }
    } finally {
      controller.dispose();
    }
  });
});
