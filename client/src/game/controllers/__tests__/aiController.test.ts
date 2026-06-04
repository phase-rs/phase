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

/**
 * Regression test for issue #2012 (turn-control crash).
 *
 * CR 723.5: When a player gains control of another player's turn (Emrakul, the
 * Promised End / Worst Fears / Mindslaver), the controller — not the controlled
 * seat — submits that turn's decisions. The engine re-derives `priority_player`
 * to the authorized submitter. The AI controller previously keyed off the
 * semantic `waiting_for.data.player` (the controlled seat), scheduled the AI to
 * act for a turn it no longer controlled, and the engine rejected every
 * dispatch as `WrongPlayer`. The controller then hit its failure cap and halted
 * via `notifyEngineLost` — the reported "crash."
 */
describe("aiController turn-control authorization (issue #2012)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    dispatchAction.mockReset();
    notifyEngineLost.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  /** Priority belongs to AI seat 1, but the human (seat 0) holds the
   *  authorized submitter slot (priority_player) — i.e. the human controls
   *  the AI's turn. */
  function humanControlsAiTurnState(): GameState {
    const waitingFor = {
      type: "Priority",
      data: { player: 1 },
    } as unknown as WaitingFor;
    return {
      waiting_for: waitingFor,
      stack: [],
      has_pending_cast: false,
      // CR 723.5: engine re-derives priority_player to the authorized submitter.
      priority_player: 0,
      active_player: 1,
      turn_decision_controller: 0,
    } as unknown as GameState;
  }

  it("stays silent when a human controls the AI's turn (does not crash)", async () => {
    const getAiAction = vi.fn(async () => ({ type: "PassPriority" }) as GameAction);
    const state = humanControlsAiTurnState();
    storeState = {
      gameState: state,
      waitingFor: state.waiting_for,
      adapter: { getAiAction, getLegalActions: vi.fn() },
    };

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    const stopSpy = vi.spyOn(controller, "stop");
    controller.start();

    for (let i = 0; i < 12; i++) {
      await vi.advanceTimersByTimeAsync(1000);
      await flushMicrotasks();
    }

    // The AI must not compute or dispatch anything for a turn it doesn't control.
    expect(getAiAction).not.toHaveBeenCalled();
    expect(dispatchAction).not.toHaveBeenCalled();
    // No failure spiral, no halt.
    expect(notifyEngineLost).not.toHaveBeenCalled();
    expect(stopSpy).not.toHaveBeenCalled();

    controller.dispose();
  });

  it("acts as the authorized submitter on a normal (uncontrolled) AI turn", async () => {
    const PASS: GameAction = { type: "PassPriority" } as GameAction;
    const getAiAction = vi.fn(async () => PASS);
    // Normal turn: AI seat 1 is both the acting player and the authorized
    // submitter (no turn-control effect).
    const state = {
      waiting_for: { type: "Priority", data: { player: 1 } } as unknown as WaitingFor,
      stack: [],
      has_pending_cast: false,
      priority_player: 1,
      active_player: 1,
      turn_decision_controller: null,
    } as unknown as GameState;
    storeState = {
      gameState: state,
      waitingFor: state.waiting_for,
      adapter: { getAiAction, getLegalActions: vi.fn() },
    };
    dispatchAction.mockResolvedValue(undefined);

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();

    for (let i = 0; i < 4; i++) {
      await vi.advanceTimersByTimeAsync(1000);
      await flushMicrotasks();
    }

    // The AI acted, dispatching as seat 1 (the authorized submitter).
    expect(getAiAction).toHaveBeenCalled();
    expect(dispatchAction).toHaveBeenCalled();
    expect(dispatchAction.mock.calls.every(([, playerId]) => playerId === 1)).toBe(true);

    controller.dispose();
  });

  it("acts as the controller when an AI controls the human's turn", async () => {
    const PASS: GameAction = { type: "PassPriority" } as GameAction;
    const getAiAction = vi.fn(async () => PASS);
    // CR 723.5: AI seat 1 cast Emrakul/Mindslaver on the human (seat 0). The
    // human's turn is active (data.player = 0), but the engine routes the
    // authorized submitter to the controller (priority_player = 1). The AI must
    // act for, and dispatch as, the controller seat — not bail because
    // data.player is the local human (which previously soft-stalled the turn).
    const state = {
      waiting_for: { type: "Priority", data: { player: 0 } } as unknown as WaitingFor,
      stack: [],
      has_pending_cast: false,
      priority_player: 1,
      active_player: 0,
      turn_decision_controller: 1,
    } as unknown as GameState;
    storeState = {
      gameState: state,
      waitingFor: state.waiting_for,
      adapter: { getAiAction, getLegalActions: vi.fn() },
    };
    dispatchAction.mockResolvedValue(undefined);

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();

    for (let i = 0; i < 4; i++) {
      await vi.advanceTimersByTimeAsync(1000);
      await flushMicrotasks();
    }

    expect(getAiAction).toHaveBeenCalled();
    expect(dispatchAction).toHaveBeenCalled();
    // Dispatched as the controller seat (1), never as the controlled human (0).
    expect(dispatchAction.mock.calls.every(([, playerId]) => playerId === 1)).toBe(true);

    controller.dispose();
  });
});
