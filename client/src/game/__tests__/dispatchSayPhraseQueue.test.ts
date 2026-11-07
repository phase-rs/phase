/**
 * Regression: two rapid sayings of Toy Boat's timed spoken payment must reach
 * the engine as two DISTINCT host observations.
 *
 * `dispatch.ts` drops a repeated same-action/same-actor/same-prompt submission
 * while the first is in flight. When every saying was the same boolean
 * `PayUnlessCost { pay: true }`, the second click was silently dropped: the
 * local count believed two sayings were reported, the engine accepted one, and
 * the payment stranded (one saying in flight, one still owed, and the client
 * refusing further clicks). A saying now carries its own host sequence and
 * instant, so the second click is a new decision that must be queued and
 * submitted. This test drives the REAL `dispatchAction` queue — not a mocked
 * boundary — with a delayed first reply and a rapid double click.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

import type {
  EngineAdapter,
  EngineSnapshot,
  GameAction,
  GameState,
  LegalActionsResult,
  SayPhrasePayment,
  SubmitResult,
  UnlessCost,
  WaitingFor,
} from "../../adapter/types";
import { nextSnapshotSeq } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { buildGameState, buildLegalActionsResult } from "../../test/factories/gameStateFactory";
import { dispatchAction } from "../dispatch";

const PAYMENT_ID = 9;
const OWED = 2;

function prompt(said: number): WaitingFor {
  const cost: UnlessCost = {
    type: "SayPhrase",
    phrase: "Toy Boat",
    pace: { type: "Quickly", max_gap_ms: 1_000 },
    repetitions: OWED - said,
  };
  return {
    type: "UnlessPayment",
    data: {
      player: 0,
      cost,
      pending_effect: {},
      effect_description: "Sacrifice Toy Boat",
    },
  } as unknown as WaitingFor;
}

function payment(said: number): SayPhrasePayment {
  return { id: PAYMENT_ID, owed: OWED, said, clock_ms: 0 };
}

function stateWith(said: number): GameState {
  return buildGameState({
    waiting_for: prompt(said) as unknown as GameState["waiting_for"],
    active_player: 0,
    priority_player: 0,
    turn_number: 3,
    say_payment: payment(said),
  });
}

function say(seq: number, atMs: number): GameAction {
  return {
    type: "SayPhrase",
    data: { payment: PAYMENT_ID, seq, at_ms: atMs },
  } as unknown as GameAction;
}

function seedStore(adapter: EngineAdapter): void {
  const state = stateWith(0);
  useGameStore.setState({
    gameId: null,
    gameMode: "ai",
    adapter,
    gameState: state,
    waitingFor: state.waiting_for,
    legalActions: [],
    events: [],
    eventHistory: [],
    logHistory: [],
    nextLogSeq: 0,
    stateHistory: [],
    turnCheckpoints: [],
    lastCommittedSeq: 0,
  });
}

describe("timed spoken payment through the real dispatch queue", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
  });

  it("submits a rapid second saying instead of dropping it as a duplicate", async () => {
    let said = 0;
    let releaseFirst: () => void = () => {};
    const firstReply = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const submitAction = vi.fn(
      async (action: GameAction): Promise<SubmitResult> => {
        if (action.type !== "SayPhrase") return { events: [], log_entries: [] };
        const { seq } = action.data as { seq: number };
        // The first observation's reply is delayed, keeping the dispatch mutex
        // held while the player's second click arrives.
        if (seq === 1) await firstReply;
        said = Math.max(said, seq);
        return { events: [], log_entries: [] };
      },
    );
    const legal: LegalActionsResult = buildLegalActionsResult({ actions: [] });
    const adapter = {
      initialize: vi.fn().mockResolvedValue(undefined),
      initializeGame: vi.fn().mockResolvedValue({ events: [] } as SubmitResult),
      restoreState: vi.fn(),
      estimateBracket: vi.fn().mockResolvedValue(null),
      dispose: vi.fn(),
      submitAction,
      getState: vi.fn(async () => stateWith(said)),
      getLegalActions: vi.fn(async () => legal),
      getSnapshot: vi.fn(
        async (): Promise<EngineSnapshot> => ({
          state: stateWith(said),
          legalResult: legal,
          seq: nextSnapshotSeq(),
        }),
      ),
    } as unknown as EngineAdapter;
    seedStore(adapter);

    // Two clicks in the same tick: the first submission is in flight, so the
    // second is queued (the mutex is held synchronously).
    const first = dispatchAction(say(1, 1_000), 0);
    const second = dispatchAction(say(2, 1_100), 0);
    releaseFirst();
    await Promise.all([first, second]);

    const submitted = submitAction.mock.calls.map(([action]) => action);
    expect(submitted).toEqual([say(1, 1_000), say(2, 1_100)]);
    expect(submitAction).toHaveBeenCalledTimes(2);
  });
});
