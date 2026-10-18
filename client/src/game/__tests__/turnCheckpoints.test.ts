import { beforeEach, describe, expect, it, vi } from "vitest";

import type { EngineAdapter, GameEvent, GameState } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory";
import { buildGameState } from "../../test/factories/gameStateFactory";
import { abandonPendingDispatches, dispatchAction } from "../dispatch";

/**
 * Debug-rewind checkpoints have the same provenance requirement as undo
 * checkpoints: rendered screen states are viewer projections
 * (`wire_projection`) that the restore ingress fails closed on, so turn
 * boundaries must capture the engine-authored trusted envelope. Unlike undo
 * (pre-action), the turn checkpoint is post-action — the TurnStarted event
 * fired during this action, so "saved at turn start" is the new turn's
 * opening state.
 */
describe("turn checkpoints", () => {
  beforeEach(() => {
    // Instant animations: steps still normalize (proving the TurnStarted
    // event flows through the real pipeline) without timer waits.
    usePreferencesStore.setState({ animationSpeedMultiplier: 0 });
  });

  function seedStore(state: GameState, adapter: EngineAdapter) {
    useGameStore.setState({
      adapter,
      gameState: state,
      gameMode: "ai",
      aiSeatIds: [],
      stateHistory: [],
      turnCheckpoints: [],
      waitingFor: null,
    });
  }

  function turnStartedEvent(turnNumber: number): GameEvent {
    return { type: "TurnStarted", data: { player_id: 0, turn_number: turnNumber } } as GameEvent;
  }

  it("TurnStarted pushes the trusted post-action envelope", async () => {
    const preState = buildGameState({ turn_number: 1, stack: [] });
    const postState = buildGameState({ turn_number: 2, stack: [] });
    const undoEnvelope = { state: preState, precast_shortcut_runtime: null };
    const turnEnvelope = { state: postState, precast_shortcut_runtime: null };
    const exportPersistenceState = vi
      .fn()
      .mockResolvedValueOnce(JSON.stringify(undoEnvelope))
      .mockResolvedValue(JSON.stringify(turnEnvelope));
    const submitAction = vi.fn().mockResolvedValue({ events: [turnStartedEvent(2)] });
    const adapter = buildEngineAdapterMock(preState, {
      exportPersistenceState,
      submitAction,
      getState: vi.fn().mockResolvedValue(postState),
    });
    seedStore(preState, adapter);

    await dispatchAction({ type: "PassPriority" }, 0);

    // Two captures with the submit strictly between: undo is pre-action,
    // the turn checkpoint is post-action.
    expect(exportPersistenceState).toHaveBeenCalledTimes(2);
    const [undoCapture, turnCapture] = exportPersistenceState.mock.invocationCallOrder;
    const [submit] = submitAction.mock.invocationCallOrder;
    expect(undoCapture).toBeLessThan(submit);
    expect(submit).toBeLessThan(turnCapture);
    expect(useGameStore.getState().stateHistory).toEqual([undoEnvelope]);
    expect(useGameStore.getState().turnCheckpoints).toEqual([turnEnvelope]);
  });

  it("action without TurnStarted leaves turnCheckpoints untouched", async () => {
    const state = buildGameState({ turn_number: 1, stack: [] });
    const envelope = { state, precast_shortcut_runtime: null };
    const adapter = buildEngineAdapterMock(state, {
      exportPersistenceState: vi.fn().mockResolvedValue(JSON.stringify(envelope)),
    });
    seedStore(state, adapter);

    await dispatchAction({ type: "PassPriority" }, 0);

    expect(useGameStore.getState().stateHistory).toHaveLength(1);
    expect(useGameStore.getState().turnCheckpoints).toHaveLength(0);
  });

  it("TurnStarted without trusted export skips the boundary without failing the action", async () => {
    const preState = buildGameState({ turn_number: 1, stack: [] });
    const postState = buildGameState({ turn_number: 2, stack: [] });
    const submitAction = vi.fn().mockResolvedValue({ events: [turnStartedEvent(2)] });
    const adapter = buildEngineAdapterMock(preState, {
      submitAction,
      getState: vi.fn().mockResolvedValue(postState),
    });
    seedStore(preState, adapter);

    await dispatchAction({ type: "PassPriority" }, 0);

    expect(submitAction).toHaveBeenCalledOnce();
    expect(useGameStore.getState().turnCheckpoints).toHaveLength(0);
    expect(useGameStore.getState().gameState).toBe(postState);
  });

  it("TurnStarted capture resolving after a session boundary is discarded", async () => {
    const preState = buildGameState({ turn_number: 1, stack: [] });
    const postState = buildGameState({ turn_number: 2, stack: [] });
    let resolveTurnExport!: (json: string) => void;
    const turnExport = new Promise<string>((resolve) => {
      resolveTurnExport = resolve;
    });
    const exportPersistenceState = vi
      .fn()
      .mockResolvedValueOnce(JSON.stringify({ state: preState, precast_shortcut_runtime: null }))
      .mockReturnValue(turnExport);
    const adapter = buildEngineAdapterMock(preState, {
      exportPersistenceState,
      submitAction: vi.fn().mockResolvedValue({ events: [turnStartedEvent(2)] }),
      getState: vi.fn().mockResolvedValue(postState),
    });
    seedStore(preState, adapter);

    const inFlight = dispatchAction({ type: "PassPriority" }, 0);
    // Wait for the turn capture to start, then turn the session over while
    // it is in flight: the resolving checkpoint must be discarded.
    await vi.waitFor(() => expect(exportPersistenceState).toHaveBeenCalledTimes(2));
    abandonPendingDispatches();
    resolveTurnExport(JSON.stringify({ state: postState, precast_shortcut_runtime: null }));
    await inFlight;

    expect(useGameStore.getState().turnCheckpoints).toHaveLength(0);
  });
});
