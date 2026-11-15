import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { EngineAdapter, EngineSnapshot, GameAction, InteractionSubmission, SubmitResult } from "../../adapter/types";
import { nextSnapshotSeq } from "../../adapter/types";
import { abandonPendingDispatches, dispatchAction, dispatchInteraction, isDispatchIdle } from "../dispatch";
import { clearPromptOverlayState } from "../sessionCleanup";
import { nextGameSessionGeneration, useGameStore } from "../../stores/gameStore";
import { useAppNotificationStore } from "../../stores/appToastStore";
import { useUiStore } from "../../stores/uiStore";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory";
import { buildGameState, buildLegalActionsResult, buildManaPaymentWaitingFor } from "../../test/factories/gameStateFactory";

describe("clearPromptOverlayState", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    useUiStore.setState({
      pendingAbilityChoice: null,
      enchantmentsDialogPlayer: null,
      manualManaOverride: false,
      mobileHandGesture: null,
      scryOutcome: null,
    });
  });

  it("clears convoke ManaPayment and UI dialogs without disposing the adapter", () => {
    const adapter = { dispose: () => {} };
    const waitingFor = buildManaPaymentWaitingFor({
      data: { player: 0, convoke_mode: "Convoke" },
    });
    useGameStore.setState({
      adapter: adapter as never,
      waitingFor,
      legalActions: [{ type: "PassPriority" }],
      autoPassRecommended: true,
      spellCosts: { "1": { type: "Cost", shards: ["G"], generic: 0 } },
      legalActionsByObject: { 1: [{ type: "TapForConvoke", data: { object_id: 1, mana_type: "Green" } }] },
      gameState: buildGameState({ waiting_for: waitingFor }),
    });
    useUiStore.setState({
      pendingAbilityChoice: {
        objectId: 1,
        actions: [{ type: "TapForConvoke", data: { object_id: 1, mana_type: "Green" } }],
      },
      enchantmentsDialogPlayer: 0,
    });

    clearPromptOverlayState();

    const state = useGameStore.getState();
    expect(state.waitingFor).toBeNull();
    expect(state.legalActions).toEqual([]);
    expect(state.autoPassRecommended).toBe(false);
    expect(state.spellCosts).toEqual({});
    expect(state.legalActionsByObject).toEqual({});
    expect(state.adapter).toBe(adapter);
    expect(state.gameState).not.toBeNull();
    expect(useUiStore.getState().pendingAbilityChoice).toBeNull();
    expect(useUiStore.getState().enchantmentsDialogPlayer).toBeNull();
  });

  it("resets the per-game manualManaOverride toggle so it can't leak across games", () => {
    useUiStore.setState({ manualManaOverride: true });

    clearPromptOverlayState();

    expect(useUiStore.getState().manualManaOverride).toBe(false);
  });

  it("resets the ephemeral hand hide-filter so it can't leak across games", () => {
    useUiStore.setState({ handFilter: "playable" });

    clearPromptOverlayState();

    expect(useUiStore.getState().handFilter).toBe("none");
  });

  it("clears an in-flight mobile hand gesture at a game boundary", () => {
    useUiStore.setState({
      mobileHandGesture: {
        objectId: 1,
        phase: "drag",
        sourceOrigin: {
          bottom: 180,
          centerX: 50,
          height: 140,
          rotation: 0,
          top: 40,
          width: 100,
        },
        offsetX: 12,
        offsetY: -80,
        playable: true,
        castReady: true,
      },
    });

    clearPromptOverlayState();

    expect(useUiStore.getState().mobileHandGesture).toBeNull();
  });

  it("clears active and queued roll overlays at a game boundary", () => {
    useUiStore.setState({
      diceRoll: { kind: "coin", playerId: 1, won: true, context: "ability" },
      diceRollQueue: [{ kind: "coin", playerId: 1, won: false, context: "ability" }],
    });

    clearPromptOverlayState();

    expect(useUiStore.getState().diceRoll).toBeNull();
    expect(useUiStore.getState().diceRollQueue).toEqual([]);
  });

  it("clears a completed scry overlay at a game boundary", () => {
    useUiStore.setState({
      scryOutcome: { playerId: 1, topCount: 2, bottomCount: 1 },
    });

    clearPromptOverlayState();

    expect(useUiStore.getState().scryOutcome).toBeNull();
  });
});

/**
 * The dispatch mutex (`isAnimating` in dispatch.ts) is module-level and shared
 * by local dispatches and inbound remote updates. A submit promise that never
 * settles holds it forever, so before this wiring one wedged dispatch froze the
 * whole page session — including the NEXT game, from its first click.
 */
describe("clearPromptOverlayState dispatch-pipeline recovery", () => {
  const passPriority = { type: "PassPriority", data: {} } as unknown as GameAction;
  const concede = { type: "Concede", data: { player_id: 0 } } as unknown as GameAction;
  const interactionSubmission: InteractionSubmission = {
    interactionId: "interaction-1" as InteractionSubmission["interactionId"],
    response: { type: "number", data: { value: 1 } },
  };

  beforeEach(() => {
    useGameStore.getState().reset();
  });

  afterEach(() => {
    // These tests deliberately leave an unsettled submit holding the mutex.
    // Release it so the wedge cannot leak into a later test in this file.
    abandonPendingDispatches();
  });

  it("releases a dispatch mutex wedged by an unsettled submit, so the next session's first action runs", () => {
    // A submit that never settles: `dispatchActionInternal`'s `finally` never
    // runs, so `isAnimating` stays held with nothing to release it.
    const submitAction = vi.fn<EngineAdapter["submitAction"]>(
      () => new Promise<SubmitResult>(() => {}),
    );
    const state = buildGameState({ stack: [], players: [] });
    useGameStore.setState({
      adapter: buildEngineAdapterMock(state, { submitAction }),
      gameState: state,
      gameMode: "ai",
    });

    void dispatchAction(passPriority, 0);
    expect(submitAction).toHaveBeenCalledTimes(1);
    expect(isDispatchIdle()).toBe(false);

    clearPromptOverlayState();

    expect(isDispatchIdle()).toBe(true);

    // Discriminating: with the mutex still held, a distinct action is pushed
    // onto `pendingQueue` and `submitAction` is never reached a second time.
    void dispatchAction(concede, 0);
    expect(submitAction).toHaveBeenCalledTimes(2);
  });

  it("drops a commit that was in flight when the boundary fired, so it cannot re-populate prompts", async () => {
    let releaseSubmit!: (result: SubmitResult) => void;
    const submitAction = vi.fn<EngineAdapter["submitAction"]>(
      () => new Promise<SubmitResult>((resolve) => {
        releaseSubmit = resolve;
      }),
    );
    const state = buildGameState({ stack: [], players: [] });
    useGameStore.setState({
      adapter: buildEngineAdapterMock(state, { submitAction }),
      gameState: state,
      gameMode: "ai",
    });

    const inFlight = dispatchAction(passPriority, 0);
    expect(submitAction).toHaveBeenCalledTimes(1);

    // The session boundary lands while the engine round-trip is outstanding.
    clearPromptOverlayState();
    expect(useGameStore.getState().waitingFor).toBeNull();

    releaseSubmit({ events: [] });
    await inFlight;

    // The commit's generation is now stale, so `isDispatchContextCurrent`
    // declines it rather than writing the prompt back over the cleared state.
    expect(useGameStore.getState().waitingFor).toBeNull();

    // Control: the same harness DOES commit a prompt when no boundary
    // intervenes, so the assertions above are not vacuously green.
    const uninterrupted = dispatchAction(concede, 0);
    releaseSubmit({ events: [] });
    await uninterrupted;

    expect(useGameStore.getState().waitingFor).toEqual(state.waiting_for);
  });

  it("commits an uninterrupted interaction reply", async () => {
    const beforeInteraction = buildGameState({ turn_number: 1 });
    const afterInteraction = buildGameState({ turn_number: 2 });
    const submitInteraction = vi.fn<NonNullable<EngineAdapter["submitInteraction"]>>()
      .mockResolvedValue({ events: [] });
    const adapter = buildEngineAdapterMock(beforeInteraction, {
      submitInteraction,
      getSnapshot: vi.fn(async () => ({
        state: afterInteraction,
        legalResult: buildLegalActionsResult(),
        seq: nextSnapshotSeq(),
      })),
    });
    useGameStore.setState({ adapter, gameState: beforeInteraction, gameMode: "ai" });

    await expect(dispatchInteraction(interactionSubmission, 0)).resolves.toEqual({ status: "applied" });

    expect(submitInteraction).toHaveBeenCalledOnce();
    expect(useGameStore.getState().gameState).toEqual(afterInteraction);
  });

  it("does not commit a delayed interaction snapshot over a replacement session", async () => {
    let releaseOldSubmit!: (result: SubmitResult) => void;
    const oldState = buildGameState({ turn_number: 3 });
    const replacementState = buildGameState({ turn_number: 8 });
    const oldGetSnapshot = vi.fn(async () => ({
      state: oldState,
      legalResult: buildLegalActionsResult(),
      seq: nextSnapshotSeq(),
    }));
    const oldSubmitInteraction = vi.fn<NonNullable<EngineAdapter["submitInteraction"]>>(
      () => new Promise<SubmitResult>((resolve) => {
        releaseOldSubmit = resolve;
      }),
    );
    const oldAdapter = buildEngineAdapterMock(oldState, {
      submitInteraction: oldSubmitInteraction,
      getSnapshot: oldGetSnapshot,
    });
    useGameStore.setState({ adapter: oldAdapter, gameState: oldState, gameMode: "ai" });

    const delayedInteraction = dispatchInteraction(interactionSubmission, 0);
    expect(oldSubmitInteraction).toHaveBeenCalledOnce();

    // This is the production session-boundary invalidation; the next game then
    // installs its own adapter, generation, and committed engine snapshot.
    clearPromptOverlayState();
    const replacementGeneration = nextGameSessionGeneration();
    const replacementAdapter = buildEngineAdapterMock(replacementState);
    useGameStore.setState({
      adapter: replacementAdapter,
      gameState: replacementState,
      gameMode: "ai",
      gameSessionGeneration: replacementGeneration,
    });
    useGameStore.getState().commitEngineSnapshot(await replacementAdapter.getSnapshot());
    expect(useGameStore.getState().gameState).toEqual(replacementState);

    // The old adapter completes after the replacement and returns its own old
    // snapshot. The dispatch must not write that result into the new session.
    releaseOldSubmit({ events: [] });
    await expect(delayedInteraction).resolves.toEqual({ status: "stale" });

    expect(oldGetSnapshot).not.toHaveBeenCalled();
    expect(useGameStore.getState().adapter).toBe(replacementAdapter);
    expect(useGameStore.getState().gameSessionGeneration).toBe(replacementGeneration);
    expect(useGameStore.getState().gameState).toEqual(replacementState);
  });

  it("drops a delayed interaction after clear without replacing its adapter", async () => {
    let releaseSubmit!: (result: SubmitResult) => void;
    const state = buildGameState({ turn_number: 6 });
    const oldGetSnapshot = vi.fn<EngineAdapter["getSnapshot"]>().mockResolvedValue({
      state,
      legalResult: buildLegalActionsResult(),
      seq: nextSnapshotSeq(),
    });
    const submitInteraction = vi.fn<NonNullable<EngineAdapter["submitInteraction"]>>(
      () => new Promise<SubmitResult>((resolve) => {
        releaseSubmit = resolve;
      }),
    );
    const adapter = buildEngineAdapterMock(state, {
      submitInteraction,
      getSnapshot: oldGetSnapshot,
    });
    useGameStore.setState({ adapter, gameState: state, gameMode: "ai" });
    const generation = useGameStore.getState().gameSessionGeneration;

    const delayedInteraction = dispatchInteraction(interactionSubmission, 0);
    expect(submitInteraction).toHaveBeenCalledOnce();

    clearPromptOverlayState();
    expect(useGameStore.getState().adapter).toBe(adapter);
    expect(useGameStore.getState().gameSessionGeneration).toBe(generation);

    releaseSubmit({ events: [] });
    await expect(delayedInteraction).resolves.toEqual({ status: "stale" });

    expect(oldGetSnapshot).not.toHaveBeenCalled();
    expect(useGameStore.getState().waitingFor).toBeNull();
    expect(useGameStore.getState().adapter).toBe(adapter);
    expect(useGameStore.getState().gameSessionGeneration).toBe(generation);
  });

  it("drops an interaction snapshot if replacement happens while fetching it", async () => {
    let releaseOldSnapshot!: (snapshot: EngineSnapshot) => void;
    const oldState = buildGameState({ turn_number: 4 });
    const replacementState = buildGameState({ turn_number: 9 });
    const oldGetSnapshot = vi.fn<EngineAdapter["getSnapshot"]>(
      () => new Promise<EngineSnapshot>((resolve) => {
        releaseOldSnapshot = resolve;
      }),
    );
    const oldAdapter = buildEngineAdapterMock(oldState, {
      submitInteraction: vi.fn<NonNullable<EngineAdapter["submitInteraction"]>>()
        .mockResolvedValue({ events: [] }),
      getSnapshot: oldGetSnapshot,
    });
    useGameStore.setState({ adapter: oldAdapter, gameState: oldState, gameMode: "ai" });

    const delayedInteraction = dispatchInteraction(interactionSubmission, 0);
    await Promise.resolve();
    expect(oldGetSnapshot).toHaveBeenCalledOnce();

    clearPromptOverlayState();
    const replacementGeneration = nextGameSessionGeneration();
    const replacementAdapter = buildEngineAdapterMock(replacementState);
    useGameStore.setState({
      adapter: replacementAdapter,
      gameState: replacementState,
      gameMode: "ai",
      gameSessionGeneration: replacementGeneration,
    });
    useGameStore.getState().commitEngineSnapshot(await replacementAdapter.getSnapshot());
    expect(useGameStore.getState().gameState).toEqual(replacementState);

    releaseOldSnapshot({
      state: oldState,
      legalResult: buildLegalActionsResult(),
      seq: nextSnapshotSeq(),
    });
    await expect(delayedInteraction).resolves.toEqual({ status: "stale" });

    expect(useGameStore.getState().adapter).toBe(replacementAdapter);
    expect(useGameStore.getState().gameSessionGeneration).toBe(replacementGeneration);
    expect(useGameStore.getState().gameState).toEqual(replacementState);
  });

  it("suppresses an interaction rejection from a replaced session", async () => {
    let rejectOldSubmit!: (error: Error) => void;
    const oldState = buildGameState({ turn_number: 5 });
    const replacementState = buildGameState({ turn_number: 10 });
    const oldSubmitInteraction = vi.fn<NonNullable<EngineAdapter["submitInteraction"]>>(
      () => new Promise<SubmitResult>((_resolve, reject) => {
        rejectOldSubmit = reject;
      }),
    );
    const oldAdapter = buildEngineAdapterMock(oldState, {
      submitInteraction: oldSubmitInteraction,
    });
    useGameStore.setState({ adapter: oldAdapter, gameState: oldState, gameMode: "ai" });
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });

    const delayedInteraction = dispatchInteraction(interactionSubmission, 0);
    expect(oldSubmitInteraction).toHaveBeenCalledOnce();

    clearPromptOverlayState();
    const replacementGeneration = nextGameSessionGeneration();
    const replacementAdapter = buildEngineAdapterMock(replacementState);
    useGameStore.setState({
      adapter: replacementAdapter,
      gameState: replacementState,
      gameMode: "ai",
      gameSessionGeneration: replacementGeneration,
    });
    useGameStore.getState().commitEngineSnapshot(await replacementAdapter.getSnapshot());

    rejectOldSubmit(new Error("old session interaction failed"));
    await expect(delayedInteraction).resolves.toEqual({ status: "stale" });

    expect(useAppNotificationStore.getState().notification).toBeNull();
    expect(useGameStore.getState().adapter).toBe(replacementAdapter);
    expect(useGameStore.getState().gameSessionGeneration).toBe(replacementGeneration);
    expect(useGameStore.getState().gameState).toEqual(replacementState);
  });

  it("reports and rethrows a rejection while its session is still current", async () => {
    const state = buildGameState({ turn_number: 11 });
    const currentError = new Error("current session interaction failed");
    const submitInteraction = vi.fn<NonNullable<EngineAdapter["submitInteraction"]>>()
      .mockRejectedValue(currentError);
    const adapter = buildEngineAdapterMock(state, { submitInteraction });
    useGameStore.setState({ adapter, gameState: state, gameMode: "ai" });
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });

    await expect(dispatchInteraction(interactionSubmission, 0)).rejects.toBe(currentError);

    expect(submitInteraction).toHaveBeenCalledOnce();
    expect(useAppNotificationStore.getState().notification).toMatchObject({
      description: currentError.message,
    });
    expect(useGameStore.getState().adapter).toBe(adapter);
    expect(useGameStore.getState().gameState).toEqual(state);
  });
});
