import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { EngineAdapter, GameAction, GameState, LegalActionsResult, SubmitResult } from "../../adapter/types";
import { AdapterError, AdapterErrorCode } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { dispatchAction } from "../dispatch";

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
const emptyState = {
  stack: [],
  waiting_for: null,
  players: [],
} as unknown as GameState;

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
  });

  beforeEach(() => {
    vi.restoreAllMocks();
    notifyEngineLost.mockClear();
  });

  it("surfaces recovery and releases the dispatch mutex so a later dispatch is not silently dropped", async () => {
    const submitAction = vi
      .fn<EngineAdapter["submitAction"]>()
      .mockRejectedValue(
        new AdapterError(AdapterErrorCode.ENGINE_UNRESPONSIVE, "worker did not respond", true),
      );

    useGameStore.setState({
      adapter: { submitAction } as unknown as EngineAdapter,
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

    // The mutex must be free: a second, distinct dispatch reaches submitAction
    // again rather than being queued behind a stuck `isAnimating`. (A held
    // mutex would queue actionB without ever calling submitAction.)
    await expect(dispatchAction(actionB, 0)).rejects.toMatchObject({
      code: AdapterErrorCode.ENGINE_UNRESPONSIVE,
    });

    expect(submitAction).toHaveBeenCalledTimes(2);
  });

  it("does not fire recovery on a normal successful dispatch", async () => {
    const submitAction = vi
      .fn<EngineAdapter["submitAction"]>()
      .mockResolvedValue({ events: [], log_entries: [] } as unknown as SubmitResult);
    const getState = vi
      .fn<EngineAdapter["getState"]>()
      .mockResolvedValue(emptyState);
    const getLegalActions = vi
      .fn<EngineAdapter["getLegalActions"]>()
      .mockResolvedValue({ actions: [], autoPassRecommended: false } as unknown as LegalActionsResult);

    useGameStore.setState({
      adapter: { submitAction, getState, getLegalActions } as unknown as EngineAdapter,
      gameState: emptyState,
      gameMode: "ai",
    });

    const action = { type: "PassPriority", data: {} } as unknown as GameAction;

    await expect(dispatchAction(action, 0)).resolves.toBeUndefined();

    // The healthy path must never surface the engine-lost recovery prompt.
    expect(notifyEngineLost).not.toHaveBeenCalled();
  });

  it("drops a queued local action when the waiting prompt changes before it runs", async () => {
    const firstWaitingFor = { type: "Priority", data: { player: 0 } };
    const nextWaitingFor = { type: "Priority", data: { player: 1 } };
    const initialState = {
      ...emptyState,
      waiting_for: firstWaitingFor,
      players: [{ id: 0 }, { id: 1 }],
      objects: { 1: { id: 1 } },
    } as unknown as GameState;
    const nextState = {
      ...initialState,
      waiting_for: nextWaitingFor,
      priority_player: 1,
    } as unknown as GameState;
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
    const getState = vi
      .fn<EngineAdapter["getState"]>()
      .mockResolvedValue(nextState);
    const getLegalActions = vi
      .fn<EngineAdapter["getLegalActions"]>()
      .mockResolvedValue({
        actions: [{ type: "SelectCards", data: { cards: [] } }],
        autoPassRecommended: false,
      } as unknown as LegalActionsResult);

    useGameStore.setState({
      adapter: { submitAction, getState, getLegalActions } as unknown as EngineAdapter,
      gameState: initialState,
      waitingFor: firstWaitingFor as unknown as GameState["waiting_for"],
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
