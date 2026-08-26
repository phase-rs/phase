/**
 * Regression (#6431): "I can't pay the sacrifice a mountain cost on Lava Dart
 * while controlling my opponent with Emrakul, the Promised End."
 *
 * CR 723.5: under a turn-control effect (Emrakul, the Promised End;
 * Mindslaver) the controller makes the controlled player's decisions. The
 * engine models this by keeping the WaitingFor's semantic `player` field on
 * the controlled seat while re-deriving `gameState.priority_player` to the
 * authorized submitter (`sync_priority_player_from_waiting_for` in
 * `public_state.rs`, for every single-actor WaitingFor variant).
 *
 * `dispatch.ts`'s `waitingForActorMatches` — used by `queuedLocalActionStillApplies`
 * to decide whether a queued local action is still valid once the animation
 * mutex releases — only consulted `gameState.priority_player` for the
 * `"Priority"` WaitingFor variant. Every other single-actor variant (notably
 * `PayCost`, the prompt Lava Dart's flashback "Sacrifice a Mountain" cost
 * uses) fell through to a bare `fields.player === actor` check. Under
 * Emrakul's turn control that field names the controlled seat (P1), not the
 * controller actually submitting the action (P0) — so the queued
 * `SelectCards` response sacrificing the Mountain was misjudged "stale" and
 * silently dropped, exactly matching "I click the Mountain and nothing
 * happens."
 *
 * This test drives the REAL `dispatchAction` pipeline: P0 casts the
 * flashback spell (as P1's authorized submitter), and — while that action's
 * animation window is still open (so the dispatch mutex is held) — P0
 * immediately submits the Mountain as the sacrifice cost target. That second
 * action must reach `adapter.submitAction`, not be dropped.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

import type {
  EngineAdapter,
  EngineSnapshot,
  GameAction,
  GameState,
  LegalActionsResult,
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

const MOUNTAIN_ID = 10;

// P0 controls P1's turn (CR 723): the flashback cast and its PayCost prompt
// are both P1's decisions, submitted by P0.
const PRIORITY = buildPriorityWaitingFor({ data: { player: 1 } });

function buildPayCostWaitingFor(): GameState["waiting_for"] {
  // A fresh object every call — mirrors a real commit producing a
  // brand-new (structurally equal) WaitingFor, so `Object.is` genuinely
  // differs from whatever was captured when the second action was queued.
  return {
    type: "PayCost",
    data: {
      player: 1,
      kind: "Sacrifice",
      choices: [MOUNTAIN_ID],
      count: 1,
      min_count: 1,
      resume: {
        type: "Spell",
        data: {
          spell: {
            object_id: 1,
            card_id: 1,
            ability: { targets: [] },
            cost: { type: "NoCost" },
          },
        },
      },
    },
  } as unknown as GameState["waiting_for"];
}

const CAST_ACTION = {
  type: "CastSpell",
  data: { object_id: 1, card_id: 1, targets: [], payment_mode: "Auto" },
} as unknown as GameAction;

const SACRIFICE_ACTION = {
  type: "SelectCards",
  data: { cards: [MOUNTAIN_ID] },
} as unknown as GameAction;

const PRIORITY_STATE = buildGameState({
  waiting_for: PRIORITY,
  active_player: 1,
  priority_player: 0,
  turn_number: 3,
});

const PRIORITY_LEGAL = buildLegalActionsResult({ actions: [CAST_ACTION] });
const PAY_COST_LEGAL = buildLegalActionsResult({ actions: [SACRIFICE_ACTION] });

/**
 * An engine that advances Priority{player:1} -> PayCost{player:1} once, on a
 * schedule the test controls (`advance()`) — modelling the flashback cast
 * resolving into its sacrifice-cost prompt during the first action's
 * animation window.
 */
function fakeEngine() {
  let advanced = false;
  return {
    advance: () => {
      advanced = true;
    },
    state: (): GameState =>
      advanced
        ? buildGameState({
            waiting_for: buildPayCostWaitingFor(),
            active_player: 1,
            priority_player: 0,
            turn_number: 3,
          })
        : PRIORITY_STATE,
    legal: (): LegalActionsResult => (advanced ? PAY_COST_LEGAL : PRIORITY_LEGAL),
  };
}

function seedStore(adapter: EngineAdapter): void {
  useGameStore.setState({
    gameId: null,
    gameMode: "ai",
    adapter,
    gameState: PRIORITY_STATE,
    waitingFor: PRIORITY,
    legalActions: [CAST_ACTION],
    events: [],
    eventHistory: [],
    logHistory: [],
    nextLogSeq: 0,
    stateHistory: [],
    turnCheckpoints: [],
    lastCommittedSeq: 0,
  });
}

const baseAdapter = (): Pick<
  EngineAdapter,
  "initialize" | "initializeGame" | "restoreState" | "estimateBracket" | "dispose"
> => ({
  initialize: vi.fn().mockResolvedValue(undefined),
  initializeGame: vi.fn().mockResolvedValue({ events: [] } as SubmitResult),
  restoreState: vi.fn(),
  estimateBracket: vi.fn().mockResolvedValue(null),
  dispose: vi.fn(),
});

describe("turn-control PayCost queueing (#6431)", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    // A non-zero multiplier keeps a real animation window open across the
    // second dispatch call, which is the window the queued action lands in.
    usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
    vi.useFakeTimers();
  });

  it("submits a queued sacrifice-cost response instead of dropping it as stale", async () => {
    const engine = fakeEngine();
    const adapter: EngineAdapter = {
      ...baseAdapter(),
      submitAction: vi.fn(async (): Promise<SubmitResult> => ({
        events: [{ type: "LifeChanged", data: { player_id: 1, amount: -3 } }],
        log_entries: [],
      })),
      getState: vi.fn(async () => engine.state()),
      getLegalActions: vi.fn(async () => engine.legal()),
      getSnapshot: vi.fn(async (): Promise<EngineSnapshot> => ({
        state: engine.state(),
        legalResult: engine.legal(),
        seq: nextSnapshotSeq(),
      })),
    };
    seedStore(adapter);

    // P0 casts the flashback spell as P1's authorized submitter. The engine
    // resolves the cast into its PayCost{player:1} prompt synchronously
    // within this call (before `submitAction`'s promise has even settled),
    // so it must be visible to the `getSnapshot()` fetch this dispatch is
    // about to make — advance it right away, before any microtask runs.
    const castDispatch = dispatchAction(CAST_ACTION, 0);
    engine.advance();

    // Let submitAction + getSnapshot settle; the animation window (from the
    // LifeChanged event) is now open, holding the dispatch mutex — the store
    // itself is NOT updated yet (commit happens only after the animation
    // window closes below).
    await vi.advanceTimersByTimeAsync(0);

    // P0 immediately clicks the Mountain to pay the sacrifice cost, while
    // the first action's animation window is still open — this is queued
    // (mutex held), capturing the store's still-stale `waitingFor` (Priority)
    // as `next.waitingFor`.
    const sacrificeDispatch = dispatchAction(SACRIFICE_ACTION, 0);

    await vi.runAllTimersAsync();
    await castDispatch;
    await sacrificeDispatch;

    // The queued SelectCards action must have reached the engine — not been
    // silently dropped as a stale response to a changed prompt.
    expect(adapter.submitAction).toHaveBeenCalledWith(SACRIFICE_ACTION, 0);
    expect(adapter.submitAction).toHaveBeenCalledTimes(2);
  });
});
