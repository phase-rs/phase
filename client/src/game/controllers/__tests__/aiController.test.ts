import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameState, LegalActionsResult, WaitingFor } from "../../../adapter/types";
import { buildGameState, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory";

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
import { debugLog } from "../../debugLog";

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
  const waitingFor: WaitingFor = {
    type: "DeclareAttackers",
    data: { player: 1, valid_attacker_ids: [GOADED_ID] },
  };
  return buildGameState({
    waiting_for: waitingFor,
    stack: [],
    has_pending_cast: false,
    priority_player: 1,
  });
}

function castOfferState(): GameState {
  const waitingFor: WaitingFor = {
    type: "CastOffer",
    data: {
      player: 1,
      kind: {
        type: "Cascade",
        hit_card: 300,
        exiled_misses: [],
        source_mv: 4,
      },
    },
  };
  return buildGameState({
    waiting_for: waitingFor,
    stack: [],
    has_pending_cast: false,
    priority_player: 1,
  });
}

function gollumNamedChoiceSource(gollumId: number) {
  return {
    prompt: {
      identity: {
        reference: { object_id: gollumId, incarnation: 0 },
        expected_zone: "Battlefield",
      },
      controller: 1,
      display_name: "Gollum, Scheming Guide",
    },
    binding: "ResolutionContext" as const,
  };
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
    vi.mocked(debugLog).mockReset();
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

  it("falls through to PassPriority when getLegalActions yields only PassPriority", async () => {
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

  it("uses the first legal action for CastOffer fallback instead of matching the WaitingFor type", async () => {
    const illegalAction = { type: "PassPriority" } as GameAction;
    const legalCastOfferAction = {
      type: "CascadeChoice",
      data: { choice: { type: "Decline" } },
    } as unknown as GameAction;
    const getAiAction = vi.fn(async () => illegalAction);
    const getLegalActions = vi.fn(
      async (): Promise<LegalActionsResult> => ({
        actions: [legalCastOfferAction],
        autoPassRecommended: false,
      }),
    );

    const state = castOfferState();
    storeState = {
      gameState: state,
      waitingFor: state.waiting_for,
      adapter: { getAiAction, getLegalActions },
    };

    dispatchAction.mockImplementation(async (action: GameAction) => {
      if (action.type === "CascadeChoice") return undefined;
      throw new Error("CastOffer requires a cast-offer response action");
    });

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();

    for (let i = 0; i < 12; i++) {
      await vi.advanceTimersByTimeAsync(1000);
      await flushMicrotasks();
    }

    expect(getLegalActions).toHaveBeenCalled();
    expect(dispatchAction.mock.calls).toContainEqual([legalCastOfferAction, 1]);
    expect(notifyEngineLost).not.toHaveBeenCalled();

    controller.dispose();
  });

  it("recovers via getLegalActions when getAiAction returns null without halting", async () => {
    const legalPass = { type: "PassPriority" } as GameAction;
    const getAiAction = vi.fn(async () => null);
    const getLegalActions = vi.fn(
      async (): Promise<LegalActionsResult> => ({
        actions: [legalPass],
        autoPassRecommended: false,
      }),
    );
    const state = buildGameState({
      waiting_for: buildPriorityWaitingFor({ data: { player: 1 } }),
      stack: [],
      has_pending_cast: false,
      priority_player: 1,
      active_player: 1,
    });
    storeState = {
      gameState: state,
      waitingFor: state.waiting_for,
      adapter: { getAiAction, getLegalActions },
    };
    dispatchAction.mockResolvedValue(undefined);

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    const stopSpy = vi.spyOn(controller, "stop");
    controller.start();

    for (let i = 0; i < 4; i++) {
      await vi.advanceTimersByTimeAsync(1000);
      await flushMicrotasks();
    }

    expect(getLegalActions).toHaveBeenCalled();
    expect(dispatchAction).toHaveBeenCalledWith(legalPass, 1);
    expect(notifyEngineLost).not.toHaveBeenCalled();
    expect(stopSpy).not.toHaveBeenCalled();

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
    vi.mocked(debugLog).mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  /** Priority belongs to AI seat 1, but the human (seat 0) holds the
   *  authorized submitter slot (priority_player) — i.e. the human controls
   *  the AI's turn. */
  function humanControlsAiTurnState(): GameState {
    const waitingFor = buildPriorityWaitingFor({ data: { player: 1 } });
    return buildGameState({
      waiting_for: waitingFor,
      stack: [],
      has_pending_cast: false,
      // CR 723.5: engine re-derives priority_player to the authorized submitter.
      priority_player: 0,
      active_player: 1,
      turn_decision_controller: 0,
    });
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
    const state = buildGameState({
      waiting_for: buildPriorityWaitingFor({ data: { player: 1 } }),
      stack: [],
      has_pending_cast: false,
      priority_player: 1,
      active_player: 1,
      turn_decision_controller: null,
    });
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

  it("routes a finite pre-cast shortcut offer through its proposer", async () => {
    const decline: GameAction = {
      type: "PrecastCopyShortcut",
      data: { epoch: 7, response: { type: "Decline" } },
    };
    const waitingFor: WaitingFor = {
      type: "PrecastCopyShortcutOffer",
      data: { proposer: 1, epoch: 7, route_count: 1 },
    };
    const state = buildGameState({
      waiting_for: waitingFor,
      stack: [],
      has_pending_cast: false,
      priority_player: 1,
      active_player: 1,
      turn_decision_controller: null,
    });
    const getAiAction = vi.fn(async () => decline);
    storeState = {
      gameState: state,
      waitingFor: state.waiting_for,
      adapter: { getAiAction, getLegalActions: vi.fn() },
    };
    dispatchAction.mockResolvedValue(undefined);

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();

    await vi.advanceTimersByTimeAsync(1000);
    await flushMicrotasks();

    expect(getAiAction).toHaveBeenCalledWith("Medium", 1, "PrecastCopyShortcutOffer");
    expect(dispatchAction).toHaveBeenCalledWith(decline, 1);

    controller.dispose();
  });

  it("logs the actual random card-predicate guess returned by the AI", async () => {
    const gollumId = 300;
    const guess: GameAction = {
      type: "ChooseOption",
      data: { choice: "Nonland" },
    };
    const waitingFor: WaitingFor = {
      type: "NamedChoice",
      data: {
        player: 1,
        choice_type: { CardPredicateGuess: { options: ["Land", "Nonland"] } },
        options: ["Land", "Nonland"],
        source: gollumNamedChoiceSource(gollumId),
      },
    };
    const state = buildGameState({
      waiting_for: waitingFor,
      priority_player: 1,
      active_player: 1,
      objects: {
        [gollumId]: {
          name: "Gollum, Scheming Guide",
        } as GameState["objects"][number],
      },
    });
    const getAiAction = vi.fn(async () => guess);
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

    expect(debugLog).toHaveBeenCalledWith(
      "AI player 2 randomly guesses Nonland for Gollum, Scheming Guide",
      "info",
    );

    controller.dispose();
  });

  it("ignores a delayed card-predicate guess after the prompt changes", async () => {
    const gollumId = 300;
    const guess: GameAction = {
      type: "ChooseOption",
      data: { choice: "Nonland" },
    };
    const scheduledWaitingFor: WaitingFor = {
      type: "NamedChoice",
      data: {
        player: 1,
        choice_type: { CardPredicateGuess: { options: ["Land", "Nonland"] } },
        options: ["Land", "Nonland"],
        source: gollumNamedChoiceSource(gollumId),
      },
    };
    const currentWaitingFor: WaitingFor = {
      type: "NamedChoice",
      data: {
        player: 1,
        choice_type: "Opponent",
        options: ["1"],
        source: gollumNamedChoiceSource(gollumId),
      },
    };
    const scheduledState = buildGameState({
      waiting_for: scheduledWaitingFor,
      priority_player: 1,
      active_player: 1,
    });
    const currentState = buildGameState({
      waiting_for: currentWaitingFor,
      priority_player: 1,
      active_player: 1,
    });
    const getAiAction = vi.fn(async () => guess);
    storeState = {
      gameState: scheduledState,
      waitingFor: scheduledState.waiting_for,
      adapter: { getAiAction, getLegalActions: vi.fn() },
    };
    dispatchAction.mockResolvedValue(undefined);

    const controller = createAIController({ seats: [{ playerId: 1, difficulty: "Medium" }] });
    controller.start();
    storeState = {
      ...storeState,
      gameState: currentState,
      waitingFor: currentState.waiting_for,
    };

    await vi.runOnlyPendingTimersAsync();
    await flushMicrotasks();

    expect(dispatchAction).not.toHaveBeenCalled();
    expect(debugLog).toHaveBeenCalledWith(
      expect.stringContaining("AI ignored stale ChooseOption"),
      "info",
    );

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
    const state = buildGameState({
      waiting_for: buildPriorityWaitingFor({ data: { player: 0 } }),
      stack: [],
      has_pending_cast: false,
      priority_player: 1,
      active_player: 0,
      turn_decision_controller: 1,
    });
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
