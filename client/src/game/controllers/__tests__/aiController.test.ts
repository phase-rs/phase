import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameState, LegalActionsResult, WaitingFor } from "../../../adapter/types";

/**
 * Regression test for issue #484 (P0 AI softlock).
 *
 * When the AI must declare attackers with a goaded creature, its heuristic
 * output omits the forced creature and the engine rejects it. After 3 such
 * failures the controller enters its stuck-fallback. Previously the fallback
 * hardcoded `DeclareAttackers { attacks: [] }` — which is *also* illegal under
 * CR 701.15b — so `totalFailures` reached `MAX_TOTAL_FAILURES` (6) and the
 * controller halted via `notifyEngineLost` + `stop()`: the softlock.
 *
 * The fix makes the fallback fetch a guaranteed-legal action from the engine
 * via `adapter.getLegalActions()`. This test reproduces the 3-failure path and
 * asserts the fallback now recovers instead of halting.
 */

// --- Mocks for the controller's heavy dependencies -------------------------

const dispatchAction = vi.fn<(action: GameAction, playerId: number) => Promise<unknown>>();

vi.mock("../../dispatch", () => ({
  dispatchAction: (action: GameAction, playerId: number) => dispatchAction(action, playerId),
}));

const notifyEngineLost = vi.fn();
vi.mock("../../engineRecovery", () => ({
  notifyEngineLost: (...args: unknown[]) => notifyEngineLost(...args),
  attemptStateRehydrate: vi.fn(async () => false),
  isEnginePanic: () => false,
  routePanic: vi.fn(async () => {}),
}));

vi.mock("../../debugLog", () => ({
  debugLog: vi.fn(),
}));

// Store mock: `getState()` returns the current snapshot. The controller drives
// itself via setTimeout + the `.finally()` re-invocation of checkAndSchedule,
// so the subscription listener does not need to be invoked by the test.
let storeState: {
  gameState: GameState | null;
  waitingFor: WaitingFor | null;
  adapter: unknown;
};

vi.mock("../../../stores/gameStore", () => ({
  useGameStore: {
    getState: () => storeState,
    subscribe: () => () => {},
  },
}));

import { createAIController } from "../aiController";

// --- Fixtures --------------------------------------------------------------

const GOADED_ID = 200;

/** The goad-compliant declaration the engine considers legal. */
const LEGAL_DECLARE: GameAction = {
  type: "DeclareAttackers",
  data: { attacks: [[GOADED_ID, { type: "Player", data: 0 }]] },
} as unknown as GameAction;

/** The illegal declaration the AI heuristic produces (omits the goaded creature). */
const ILLEGAL_DECLARE: GameAction = {
  type: "DeclareAttackers",
  data: { attacks: [] },
} as unknown as GameAction;

function declareAttackersState(): GameState {
  const waitingFor = {
    type: "DeclareAttackers",
    data: { player: 1, valid_attacker_ids: [GOADED_ID] },
  } as unknown as WaitingFor;
  return {
    waiting_for: waitingFor,
    stack: [],
    has_pending_cast: false,
  } as unknown as GameState;
}

/** Flush pending microtasks (promise `.then` chains). */
async function flushMicrotasks() {
  for (let i = 0; i < 10; i++) {
    await Promise.resolve();
  }
}

describe("aiController stuck-fallback (issue #484)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    dispatchAction.mockReset();
    notifyEngineLost.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("recovers via getLegalActions instead of halting on a goaded-creature softlock", async () => {
    const getAiAction = vi.fn(async () => ILLEGAL_DECLARE);
    const getLegalActions = vi.fn(
      async (): Promise<LegalActionsResult> => ({
        actions: [LEGAL_DECLARE],
        autoPassRecommended: false,
      }),
    );

    const state = declareAttackersState();
    storeState = {
      gameState: state,
      waitingFor: state.waiting_for,
      adapter: { getAiAction, getLegalActions },
    };

    // The engine rejects every illegal DeclareAttackers; it accepts the
    // goad-compliant one from getLegalActions.
    dispatchAction.mockImplementation(async (action: GameAction) => {
      const isLegal =
        action.type === "DeclareAttackers" &&
        ((action as unknown as { data: { attacks: unknown[] } }).data.attacks.length > 0);
      if (!isLegal) {
        throw new Error("CR 701.15b: goaded creature must attack");
      }
      return undefined;
    });

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    const stopSpy = vi.spyOn(controller, "stop");

    controller.start();

    // Drive the 3 normal-path failures + the fallback. Each normal attempt
    // schedules via setTimeout (AI delay), then re-invokes checkAndSchedule
    // in its .finally(). Advance timers and flush microtasks repeatedly until
    // the controller settles.
    for (let i = 0; i < 12; i++) {
      await vi.advanceTimersByTimeAsync(1000);
      await flushMicrotasks();
    }

    // The fallback dispatched the engine-legal action from getLegalActions...
    expect(getLegalActions).toHaveBeenCalled();
    const dispatchedLegal = dispatchAction.mock.calls.some(
      ([action]) =>
        action.type === "DeclareAttackers" &&
        (action as unknown as { data: { attacks: unknown[] } }).data.attacks.length > 0,
    );
    expect(dispatchedLegal).toBe(true);

    // ...and the controller never halted (no softlock).
    expect(notifyEngineLost).not.toHaveBeenCalled();
    expect(stopSpy).not.toHaveBeenCalled();

    controller.dispose();
  });

  it("falls through to PassPriority when getLegalActions yields no matching action", async () => {
    const getAiAction = vi.fn(async () => ILLEGAL_DECLARE);
    // Degenerate engine response: no DeclareAttackers entry.
    const getLegalActions = vi.fn(
      async (): Promise<LegalActionsResult> => ({
        actions: [{ type: "PassPriority" } as GameAction],
        autoPassRecommended: false,
      }),
    );

    const state = declareAttackersState();
    storeState = {
      gameState: state,
      waitingFor: state.waiting_for,
      adapter: { getAiAction, getLegalActions },
    };

    // Only PassPriority is accepted in this degenerate scenario.
    dispatchAction.mockImplementation(async (action: GameAction) => {
      if (action.type === "PassPriority") return undefined;
      throw new Error("illegal");
    });

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();

    for (let i = 0; i < 12; i++) {
      await vi.advanceTimersByTimeAsync(1000);
      await flushMicrotasks();
    }

    const dispatchedPass = dispatchAction.mock.calls.some(
      ([action]) => action.type === "PassPriority",
    );
    expect(dispatchedPass).toBe(true);
    // `undefined` is never dispatched.
    expect(dispatchAction.mock.calls.every(([action]) => action != null)).toBe(true);

    controller.dispose();
  });
});
