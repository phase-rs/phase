import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ActionRejection, EngineAdapter, GameAction, SubmitResult } from "../../adapter/types";
import { actionRejectionError, AdapterError, AdapterErrorCode, nextSnapshotSeq } from "../../adapter/types";
import { useAppNotificationStore } from "../../stores/appToastStore";
import { useGameStore } from "../../stores/gameStore";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory";
import {
  buildGameState,
  buildLegalActionsResult,
  buildPlayer,
  buildPriorityWaitingFor,
} from "../../test/factories/gameStateFactory";
import { dispatchAction, dispatchInteraction, isDispatchIdle } from "../dispatch";

// Spy on the recovery escalation so we can assert the dispatch.ts branch that
// fires `notifyEngineLost` on ENGINE_UNRESPONSIVE actually runs. Without this
// the test could not distinguish "recovery surfaced" from "error merely
// rethrown" — both reset the mutex, so the mutex assertion alone is not
// discriminating for the dispatch.ts hunk under review.
const notifyEngineLost = vi.fn();
vi.mock("../engineRecovery", () => ({
  notifyEngineLost: (...args: unknown[]) => notifyEngineLost(...args),
  // Unreachable on the ENGINE_UNRESPONSIVE path (we early-return before any
  // rehydrate), but dispatch.ts imports them, so they must exist.
  attemptStateRehydrate: vi.fn(async () => false),
  isEnginePanic: () => false,
  routePanic: vi.fn(async () => {}),
}));

/** Minimal stack-empty state — enough for dispatch's pre-call bookkeeping. */
const emptyState = buildGameState({
  stack: [],
  players: [],
});

function rejection(overrides: Partial<ActionRejection> = {}): ActionRejection {
  return {
    code: "invalid_action",
    disposition: "invalid",
    message: "Engine error: ObjectId(200) must be blocked by 2 or more creatures",
    related_object_ids: [200],
    ...overrides,
  };
}

/**
 * Regression for the silent-freeze bug: when a gameplay worker round-trip
 * wedges, `submitAction` rejects with ENGINE_UNRESPONSIVE (the watchdog
 * timeout). That rejection must (a) drive `processAction` to escalate via
 * `notifyEngineLost` so the user sees the Layer 3 recovery prompt, and
 * (b) propagate through `dispatchAction`'s finally, which resets the
 * module-level `isAnimating` mutex. If the mutex stayed held, every later
 * click would be silently queued/dropped and the UI would look dead.
 */
describe("dispatchAction recovery on ENGINE_UNRESPONSIVE", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    notifyEngineLost.mockClear();
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
  });

  beforeEach(() => {
    vi.restoreAllMocks();
    notifyEngineLost.mockClear();
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
  });

  it("surfaces recovery and releases the dispatch mutex so a later dispatch is not silently dropped", async () => {
    const submitAction = vi
      .fn<EngineAdapter["submitAction"]>()
      .mockRejectedValue(
        new AdapterError(AdapterErrorCode.ENGINE_UNRESPONSIVE, "worker did not respond", true),
      );

    useGameStore.setState({
      adapter: buildEngineAdapterMock(emptyState, { submitAction }),
      gameState: emptyState,
      gameMode: "ai",
    });

    const actionA = { type: "PassPriority", data: {} } as unknown as GameAction;
    const actionB = { type: "ConcedeGame", data: {} } as unknown as GameAction;

    // First dispatch hits the wedged worker and rejects.
    await expect(dispatchAction(actionA, 0)).rejects.toMatchObject({
      code: AdapterErrorCode.ENGINE_UNRESPONSIVE,
    });

    // Discriminating for the dispatch.ts hunk: the ENGINE_UNRESPONSIVE branch
    // must have escalated to the Layer 3 recovery prompt. Remove that branch
    // and the error is merely rethrown — this assertion then fails even though
    // the mutex still resets.
    expect(notifyEngineLost).toHaveBeenCalledWith("submitAction-timeout");
    expect(useAppNotificationStore.getState().notification).toBeNull();

    // The mutex must be free: a second, distinct dispatch reaches submitAction
    // again rather than being queued behind a stuck `isAnimating`. (A held
    // mutex would queue actionB without ever calling submitAction.)
    await expect(dispatchAction(actionB, 0)).rejects.toMatchObject({
      code: AdapterErrorCode.ENGINE_UNRESPONSIVE,
    });

    expect(submitAction).toHaveBeenCalledTimes(2);
  });

  it("returns the pipeline to idle when a P2P guest submission times out", async () => {
    // What `P2PGuestAdapter`'s submission timeout rejects with. Before it
    // existed the promise never settled, so `dispatchAction` never reached its
    // `finally` and the module-level mutex stayed held — every later click was
    // silently queued and the client sat frozen on a stale board.
    const timeout = new AdapterError(
      "P2P_ERROR",
      "The host did not answer this action in time",
      true,
    );
    const submitAction = vi.fn<EngineAdapter["submitAction"]>().mockRejectedValue(timeout);

    useGameStore.setState({
      adapter: buildEngineAdapterMock(emptyState, { submitAction }),
      gameState: emptyState,
      gameMode: "online",
    });

    await expect(dispatchAction({ type: "PassPriority", data: {} } as GameAction, 0)).rejects.toBe(
      timeout,
    );

    expect(isDispatchIdle()).toBe(true);
    // `P2P_ERROR`, not `ENGINE_UNRESPONSIVE`: the timeout must surface as a
    // normal action error, never route to the engine-lost recovery prompt —
    // the host may simply be gone, and the local engine is fine.
    expect(notifyEngineLost).not.toHaveBeenCalled();
    expect(useAppNotificationStore.getState().notification).toMatchObject({
      description: timeout.message,
    });
  });

  it("shows a clear toast when a normal game action fails", async () => {
    const submitAction = vi
      .fn<EngineAdapter["submitAction"]>()
      .mockRejectedValue(new Error("Engine error: Action not allowed: Cannot pay mana cost"));

    useGameStore.setState({
      adapter: buildEngineAdapterMock(emptyState, { submitAction }),
      gameState: emptyState,
      gameMode: "ai",
    });

    await expect(dispatchAction({ type: "ChooseTarget", data: { target: null } }, 0)).rejects.toThrow(
      "Cannot pay mana cost",
    );

    expect(useAppNotificationStore.getState().notification).toEqual({
      title: "Skip target failed",
      description: "Engine error: Action not allowed: Cannot pay mana cost",
    });
  });

  it("anchors a structured rejection at the first rendered related object without changing its message", async () => {
    const first = document.createElement("div");
    first.dataset.objectId = "200";
    first.getBoundingClientRect = () => ({
      x: 10, y: 30, top: 30, right: 50, bottom: 70, left: 10, width: 40, height: 40,
      toJSON: () => ({}),
    });
    const second = document.createElement("div");
    second.dataset.objectId = "201";
    document.body.append(first, second);
    const engineRejection = rejection({ related_object_ids: [200, 201] });
    const submitAction = vi
      .fn<EngineAdapter["submitAction"]>()
      .mockRejectedValue(actionRejectionError(engineRejection));

    useGameStore.setState({
      adapter: buildEngineAdapterMock(emptyState, { submitAction }),
      gameState: emptyState,
      gameMode: "ai",
    });

    await expect(dispatchAction({ type: "ChooseTarget", data: { target: null } }, 0)).rejects.toBeInstanceOf(AdapterError);

    expect(useAppNotificationStore.getState().notification).toEqual({
      title: "Skip target failed",
      description: engineRejection.message,
      anchor: { x: 192, y: 82, placement: "below" },
    });
    first.remove();
    second.remove();
  });

  it("silently absorbs a stale structured rejection", async () => {
    const submitAction = vi
      .fn<EngineAdapter["submitAction"]>()
      .mockRejectedValue(actionRejectionError(rejection({
        code: "stale_action",
        disposition: "stale",
        message: "The action is no longer current",
      })));

    useGameStore.setState({
      adapter: buildEngineAdapterMock(emptyState, { submitAction }),
      gameState: emptyState,
      gameMode: "ai",
    });

    await expect(dispatchAction({ type: "PassPriority", data: {} } as GameAction, 0)).resolves.toBeUndefined();
    expect(useAppNotificationStore.getState().notification).toBeNull();
  });

  it("reports structured interaction rejections through the same contextual path", async () => {
    const anchor = document.createElement("div");
    anchor.dataset.groupedIds = "201 202";
    anchor.getBoundingClientRect = () => ({
      x: 40, y: 80, top: 80, right: 140, bottom: 120, left: 40, width: 100, height: 40,
      toJSON: () => ({}),
    });
    document.body.append(anchor);
    const engineRejection = rejection({ related_object_ids: [202] });
    const submitInteraction = vi
      .fn<NonNullable<EngineAdapter["submitInteraction"]>>()
      .mockRejectedValue(actionRejectionError(engineRejection));

    useGameStore.setState({
      adapter: buildEngineAdapterMock(emptyState, { submitInteraction }),
      gameState: emptyState,
      gameMode: "ai",
    });

    await expect(dispatchInteraction({} as never, 0)).rejects.toBeInstanceOf(AdapterError);

    expect(useAppNotificationStore.getState().notification).toEqual({
      title: "Action failed",
      description: engineRejection.message,
      anchor: { x: 192, y: 132, placement: "below" },
    });
    anchor.remove();
  });

  it("reports a queued structured rejection once it reaches the dispatch pipeline", async () => {
    const engineRejection = rejection({ related_object_ids: [] });
    let resolveFirst!: (result: SubmitResult) => void;
    const submitAction = vi
      .fn<EngineAdapter["submitAction"]>()
      .mockImplementationOnce(() => new Promise<SubmitResult>((resolve) => {
        resolveFirst = resolve;
      }))
      .mockRejectedValueOnce(actionRejectionError(engineRejection));
    const getSnapshot = vi.fn<EngineAdapter["getSnapshot"]>().mockResolvedValue({
      state: emptyState,
      legalResult: buildLegalActionsResult({ actions: [{ type: "ChooseTarget", data: { target: null } }] }),
      seq: nextSnapshotSeq(),
    });

    useGameStore.setState({
      adapter: buildEngineAdapterMock(emptyState, { submitAction, getSnapshot }),
      gameState: emptyState,
      waitingFor: emptyState.waiting_for,
      legalActions: [{ type: "ChooseTarget", data: { target: null } }],
      gameMode: "ai",
    });

    const first = dispatchAction({ type: "PassPriority", data: {} } as GameAction, 0);
    const queued = dispatchAction({ type: "ChooseTarget", data: { target: null } }, 0);
    resolveFirst({ events: [], log_entries: [] } as SubmitResult);

    await expect(first).resolves.toBeUndefined();
    await expect(queued).rejects.toBeInstanceOf(AdapterError);
    expect(useAppNotificationStore.getState().notification).toMatchObject({
      title: "Skip target failed",
      description: engineRejection.message,
    });
  });

  it("does not fire recovery on a normal successful dispatch", async () => {
    const submitAction = vi
      .fn<EngineAdapter["submitAction"]>()
      .mockResolvedValue({ events: [], log_entries: [] } as unknown as SubmitResult);
    // `processAction` reads the engine pair through `getSnapshot` only — the
    // old getState + post-animation getLegalActions pair is gone.
    const getSnapshot = vi
      .fn<EngineAdapter["getSnapshot"]>()
      .mockImplementation(async () => ({
        state: emptyState,
        legalResult: buildLegalActionsResult(),
        seq: nextSnapshotSeq(),
      }));

    useGameStore.setState({
      adapter: buildEngineAdapterMock(emptyState, { submitAction, getSnapshot }),
      gameState: emptyState,
      gameMode: "ai",
    });

    const action = { type: "PassPriority", data: {} } as unknown as GameAction;

    await expect(dispatchAction(action, 0)).resolves.toBeUndefined();

    // The healthy path must never surface the engine-lost recovery prompt.
    expect(notifyEngineLost).not.toHaveBeenCalled();
    // Exactly one engine pair read per dispatch — the split-epoch second fetch
    // is gone, so a regression that reintroduces it shows up here.
    expect(getSnapshot).toHaveBeenCalledTimes(1);
  });

  it("drops a queued local action when the waiting prompt changes before it runs", async () => {
    const firstWaitingFor = buildPriorityWaitingFor();
    const nextWaitingFor = buildPriorityWaitingFor({ data: { player: 1 } });
    const initialState = buildGameState({
      waiting_for: firstWaitingFor,
      players: [buildPlayer({ id: 0 }), buildPlayer({ id: 1 })],
      objects: {},
    });
    const nextState = buildGameState({
      ...initialState,
      waiting_for: nextWaitingFor,
      priority_player: 1,
    });
    let releaseFirst!: () => void;
    const submitAction = vi
      .fn<EngineAdapter["submitAction"]>()
      .mockImplementationOnce(
        () =>
          new Promise<SubmitResult>((resolve) => {
            releaseFirst = () => resolve({ events: [], log_entries: [] } as unknown as SubmitResult);
          }),
      )
      .mockResolvedValue({ events: [], log_entries: [] } as unknown as SubmitResult);
    const getSnapshot = vi
      .fn<EngineAdapter["getSnapshot"]>()
      .mockImplementation(async () => ({
        state: nextState,
        legalResult: buildLegalActionsResult({
          actions: [{ type: "SelectCards", data: { cards: [] } }],
        }),
        seq: nextSnapshotSeq(),
      }));

    useGameStore.setState({
      adapter: buildEngineAdapterMock(initialState, { submitAction, getSnapshot }),
      gameState: initialState,
      waitingFor: firstWaitingFor,
      gameMode: "ai",
    });

    const first = dispatchAction({ type: "PassPriority" } as unknown as GameAction, 0);
    const queued = dispatchAction({ type: "SelectCards", data: { cards: [] } } as unknown as GameAction, 0);

    releaseFirst();
    await expect(Promise.all([first, queued])).resolves.toEqual([undefined, undefined]);

    expect(useGameStore.getState().waitingFor).toBe(nextWaitingFor);
    expect(submitAction).toHaveBeenCalledTimes(1);
  });
});
