/**
 * P2P host softlock — split-epoch commit regression.
 *
 * Observed (saved game-state ZIP): the host store held
 * `gameState.waiting_for = Priority{player:0}` while `legalActions` was
 * `[DecideOptionalEffect, DecideOptionalEffect]`. The engine was actually at
 * `OptionalEffectChoice`. The UI, driven by the stale `waitingFor`, rendered
 * Resolve / Resolve All — actions the engine rejected — while the Yes/No
 * optional-effect modal never mounted. The game locked permanently.
 *
 * Root cause: `processAction` fetched the state BEFORE the animation window and
 * the legal actions AFTER it, so any engine advance during that window produced
 * a pair read at two different engine versions. On the host that advance is real
 * and common: `P2PHostAdapter.runAiLoop` / `handleGuestMessage` drive the shared
 * engine outside the dispatch mutex.
 *
 * This test drives the REAL `dispatchAction` pipeline against an adapter whose
 * engine advances Priority → OptionalEffectChoice *during* the animation window,
 * and asserts the store never lands on the mixed pair.
 *
 * Discrimination: these assertions were confirmed RED against the pre-fix
 * `processAction` (state fetched at step 3b, legal actions re-fetched at step 8,
 * committed as one pair) — `storeHoldsMixedPair()` returned true and the stale
 * pair clobbered the newer commit. The third test below is a standing control:
 * it replays that old pairing directly and asserts it DOES produce the softlock
 * shape, so `storeHoldsMixedPair()` is proven non-vacuous without needing to
 * re-break the source.
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
import { nextGameSessionGeneration, useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import {
  buildGameState,
  buildLegalActionsResult,
  buildPriorityWaitingFor,
} from "../../test/factories/gameStateFactory";
import { dispatchAction, dispatchActionForGameSession } from "../dispatch";

const PRIORITY = buildPriorityWaitingFor({ data: { player: 0 } });

const OPTIONAL_EFFECT_CHOICE = {
  type: "OptionalEffectChoice",
  data: {
    player: 0,
    source_id: 100,
    description: "Ob Nixilis, the Fallen — you may have target player lose 3 life.",
  },
} as unknown as GameState["waiting_for"];

const PRIORITY_ACTIONS = [{ type: "PassPriority" }] as unknown as GameAction[];
const OPTIONAL_EFFECT_ACTIONS = [
  { type: "DecideOptionalEffect", data: { accept: true } },
  { type: "DecideOptionalEffect", data: { accept: false } },
] as unknown as GameAction[];

const PRIORITY_STATE = buildGameState({ waiting_for: PRIORITY, turn_number: 3 });
const OPTIONAL_STATE = buildGameState({ waiting_for: OPTIONAL_EFFECT_CHOICE, turn_number: 3 });

const PRIORITY_LEGAL = buildLegalActionsResult({ actions: PRIORITY_ACTIONS });
const OPTIONAL_LEGAL = buildLegalActionsResult({ actions: OPTIONAL_EFFECT_ACTIONS });

/**
 * An engine that advances Priority → OptionalEffectChoice once, on a schedule the
 * test controls (`advance()`), modelling the host's out-of-band AI-loop /
 * guest-action advance landing during the animation window.
 */
function fakeEngine() {
  let advanced = false;
  return {
    advance: () => {
      advanced = true;
    },
    state: (): GameState => (advanced ? OPTIONAL_STATE : PRIORITY_STATE),
    legal: (): LegalActionsResult => (advanced ? OPTIONAL_LEGAL : PRIORITY_LEGAL),
  };
}

/** True when the store holds the exact softlock shape from the bug report. */
function storeHoldsMixedPair(): boolean {
  const { waitingFor, legalActions } = useGameStore.getState();
  return (
    waitingFor?.type === "Priority" &&
    legalActions.some((a) => a.type === "DecideOptionalEffect")
  );
}

function seedStore(adapter: EngineAdapter): void {
  useGameStore.setState({
    gameId: null,
    gameMode: "ai",
    adapter,
    gameState: PRIORITY_STATE,
    waitingFor: PRIORITY,
    legalActions: PRIORITY_ACTIONS,
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
  "initialize" | "initializeGame" | "restoreState" | "getAiAction" | "estimateBracket" | "dispose"
> => ({
  initialize: vi.fn().mockResolvedValue(undefined),
  initializeGame: vi.fn().mockResolvedValue({ events: [] } as SubmitResult),
  restoreState: vi.fn(),
  getAiAction: vi.fn().mockReturnValue(null),
  estimateBracket: vi.fn().mockResolvedValue(null),
  dispose: vi.fn(),
});

describe("P2P host softlock — engine advance during the animation window", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    // A non-zero multiplier keeps a real animation window open, which is the
    // window the engine advance lands in. `normalizeEvents` produces a step for
    // the LifeChanged event below, so the pipeline genuinely awaits a timer.
    usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
    vi.useFakeTimers();
  });

  it("never commits state and legal actions read at different engine versions", async () => {
    const engine = fakeEngine();

    const adapter: EngineAdapter = {
      ...baseAdapter(),
      submitAction: vi.fn(async (): Promise<SubmitResult> => ({
        events: [{ type: "LifeChanged", data: { player_id: 1, amount: -3 } }],
        log_entries: [],
      })),
      getState: vi.fn(async () => engine.state()),
      getLegalActions: vi.fn(async () => engine.legal()),
      // The atomic read: BOTH halves come from the engine version live at the
      // moment of the call. This is the contract the fix depends on.
      getSnapshot: vi.fn(async (): Promise<EngineSnapshot> => ({
        state: engine.state(),
        legalResult: engine.legal(),
        seq: nextSnapshotSeq(),
      })),
    };
    seedStore(adapter);

    const dispatched = dispatchAction({ type: "PassPriority" } as GameAction, 0);

    // Let submitAction + the snapshot read settle, then advance the engine out of
    // band — exactly what `runAiLoop` does to the shared engine mid-animation.
    await vi.advanceTimersByTimeAsync(0);
    engine.advance();

    // Drain the animation window and the rest of the pipeline.
    await vi.runAllTimersAsync();
    await dispatched;

    // THE assertion: the store must never hold Priority + DecideOptionalEffect.
    expect(storeHoldsMixedPair()).toBe(false);

    // And the pair it does hold must be internally consistent — both halves from
    // the same engine version.
    const { waitingFor, legalActions, gameState } = useGameStore.getState();
    expect(waitingFor).toEqual(gameState?.waiting_for);
    if (waitingFor?.type === "OptionalEffectChoice") {
      expect(legalActions).toEqual(OPTIONAL_EFFECT_ACTIONS);
    } else {
      expect(legalActions).toEqual(PRIORITY_ACTIONS);
    }
  });

  it("drops the in-flight pair when a newer commit lands mid-animation, instead of clobbering it", async () => {
    // The other direction of the same bug: a concurrent `gameStore.dispatch`
    // (PassButton, choice modals, mana UI — ~30 components bypass the animation
    // queue) commits the NEWER pair while `processAction` is still animating. The
    // in-flight older pair must not overwrite it on arrival.
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

    const dispatched = dispatchAction({ type: "PassPriority" } as GameAction, 0);
    // `processAction` has now captured its (pre-advance) snapshot.
    await vi.advanceTimersByTimeAsync(0);

    // Engine advances, and a newer commit lands while the animation is still
    // playing — a strictly newer seq than the in-flight one.
    engine.advance();
    const newer = await adapter.getSnapshot();
    useGameStore.getState().commitEngineSnapshot(newer);
    expect(useGameStore.getState().waitingFor).toEqual(OPTIONAL_EFFECT_CHOICE);

    await vi.runAllTimersAsync();
    await dispatched;

    // The older in-flight pair was dropped by the revision gate: the store still
    // shows the newer engine version, coherently paired.
    expect(useGameStore.getState().waitingFor).toEqual(OPTIONAL_EFFECT_CHOICE);
    expect(useGameStore.getState().legalActions).toEqual(OPTIONAL_EFFECT_ACTIONS);
    expect(storeHoldsMixedPair()).toBe(false);
  });

  it("drops a delayed preference dispatch when a replacement game lifecycle takes over", async () => {
    let releaseOldSubmit!: (result: SubmitResult) => void;
    const oldGetSnapshot = vi.fn<EngineAdapter["getSnapshot"]>();
    const oldAdapter: EngineAdapter = {
      ...baseAdapter(),
      submitAction: vi.fn(
        () =>
          new Promise<SubmitResult>((resolve) => {
            releaseOldSubmit = resolve;
          }),
      ),
      getState: vi.fn(async () => PRIORITY_STATE),
      getLegalActions: vi.fn(async () => PRIORITY_LEGAL),
      getSnapshot: oldGetSnapshot,
    };
    seedStore(oldAdapter);

    const oldGeneration = useGameStore.getState().gameSessionGeneration;
    const oldDispatch = dispatchActionForGameSession(
      { type: "SetPhaseStops", data: { stops: [] } },
      oldAdapter,
      oldGeneration,
      0,
    );
    expect(oldAdapter.submitAction).toHaveBeenCalledTimes(1);

    const newGetSnapshot = vi.fn(async (): Promise<EngineSnapshot> => ({
      state: OPTIONAL_STATE,
      legalResult: OPTIONAL_LEGAL,
      seq: nextSnapshotSeq(),
    }));
    const newAdapter: EngineAdapter = {
      ...baseAdapter(),
      submitAction: vi.fn(async (): Promise<SubmitResult> => ({
        events: [],
        log_entries: [],
      })),
      getState: vi.fn(async () => OPTIONAL_STATE),
      getLegalActions: vi.fn(async () => OPTIONAL_LEGAL),
      getSnapshot: newGetSnapshot,
    };
    const newGeneration = nextGameSessionGeneration();
    useGameStore.setState({
      adapter: newAdapter,
      gameState: OPTIONAL_STATE,
      waitingFor: OPTIONAL_EFFECT_CHOICE,
      legalActions: OPTIONAL_EFFECT_ACTIONS,
      gameSessionGeneration: newGeneration,
    });

    // The replacement lifecycle may legitimately send the same preference.
    // It must not be deduplicated against the old session's in-flight action.
    const newDispatch = dispatchActionForGameSession(
      { type: "SetPhaseStops", data: { stops: [] } },
      newAdapter,
      newGeneration,
      0,
    );

    releaseOldSubmit({ events: [], log_entries: [] });
    await oldDispatch;
    await newDispatch;

    expect(oldGetSnapshot).not.toHaveBeenCalled();
    expect(newAdapter.submitAction).toHaveBeenCalledTimes(1);
    expect(newGetSnapshot).toHaveBeenCalledTimes(1);
    expect(useGameStore.getState().gameState).toEqual(OPTIONAL_STATE);
    expect(useGameStore.getState().waitingFor).toEqual(OPTIONAL_EFFECT_CHOICE);
    expect(useGameStore.getState().legalActions).toEqual(OPTIONAL_EFFECT_ACTIONS);
  });

  it("red-before-green control: the OLD split-fetch pairing produces exactly the softlock shape", async () => {
    // This test does NOT exercise the fix — it pins the bug, so the assertion in
    // the tests above is provably non-vacuous. It reproduces the pre-fix flow
    // literally: state read BEFORE the animation window, legal actions read
    // AFTER it, then committed as one pair (the old `useGameStore.setState`).
    const engine = fakeEngine();

    // Step 3b of the old flow: state fetched pre-animation (engine still Priority).
    const staleState = engine.state();

    // …the engine advances during the animation window…
    engine.advance();

    // Step 8 of the old flow: legal actions fetched post-animation (now Optional).
    const freshLegal = engine.legal();

    // The old code committed this cross-epoch pair verbatim.
    useGameStore.setState({
      gameState: staleState,
      waitingFor: staleState.waiting_for,
      legalActions: freshLegal.actions,
    });

    // That is precisely the reported softlock: waitingFor says Priority (so the
    // UI renders Resolve/Resolve All) while the engine wants DecideOptionalEffect.
    expect(storeHoldsMixedPair()).toBe(true);
    expect(useGameStore.getState().waitingFor?.type).toBe("Priority");
    expect(useGameStore.getState().legalActions.map((a) => a.type)).toEqual([
      "DecideOptionalEffect",
      "DecideOptionalEffect",
    ]);
  });
});
