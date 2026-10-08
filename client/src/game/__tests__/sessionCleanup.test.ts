import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { EngineAdapter, GameAction, SubmitResult } from "../../adapter/types";
import { abandonPendingDispatches, dispatchAction, isDispatchIdle } from "../dispatch";
import { clearPromptOverlayState } from "../sessionCleanup";
import { useGameStore } from "../../stores/gameStore";
import { useUiStore } from "../../stores/uiStore";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory";
import { buildGameState, buildManaPaymentWaitingFor } from "../../test/factories/gameStateFactory";

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
});
