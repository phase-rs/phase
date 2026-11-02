import { describe, expect, it, vi } from "vitest";

import type { EngineAdapter, GameState } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory";
import { buildGameState } from "../../test/factories/gameStateFactory";
import { dispatchAction } from "../dispatch";

/**
 * Undo checkpoints must be engine-authored trusted envelopes captured before
 * the action submits. Rendered screen states are viewer projections
 * (`wire_projection`) that the restore ingress fails closed on, so pushing
 * them made every undo reject silently while the button stayed up.
 */
describe("undo checkpoints", () => {
  function seedStore(state: GameState, adapter: EngineAdapter, aiSeatIds: number[] = []) {
    useGameStore.setState({
      adapter,
      gameState: state,
      gameMode: "ai",
      aiSeatIds,
      stateHistory: [],
      waitingFor: null,
    });
  }

  it("human undoable dispatch pushes the trusted pre-action envelope", async () => {
    const state = buildGameState({ turn_number: 1, stack: [] });
    const envelope = { state, precast_shortcut_runtime: null };
    const exportPersistenceState = vi.fn().mockResolvedValue(JSON.stringify(envelope));
    const adapter = buildEngineAdapterMock(state, { exportPersistenceState });
    seedStore(state, adapter);

    await dispatchAction({ type: "PassPriority" }, 0);

    expect(exportPersistenceState).toHaveBeenCalledOnce();
    expect(exportPersistenceState.mock.invocationCallOrder[0]).toBeLessThan(
      adapter.submitAction.mock.invocationCallOrder[0],
    );
    expect(useGameStore.getState().stateHistory).toEqual([envelope]);
  });

  it("AI-seat dispatch pushes no checkpoint but still applies", async () => {
    const state = buildGameState({ turn_number: 1, stack: [] });
    const exportPersistenceState = vi.fn().mockResolvedValue("{}");
    const adapter = buildEngineAdapterMock(state, { exportPersistenceState });
    seedStore(state, adapter, [1]);

    await dispatchAction({ type: "PassPriority" }, 1);

    expect(adapter.submitAction).toHaveBeenCalledOnce();
    expect(exportPersistenceState).not.toHaveBeenCalled();
    expect(useGameStore.getState().stateHistory).toHaveLength(0);
  });

  it("automated pass pushes no checkpoint but still applies", async () => {
    const state = buildGameState({ turn_number: 1, stack: [] });
    const exportPersistenceState = vi.fn().mockResolvedValue("{}");
    const adapter = buildEngineAdapterMock(state, { exportPersistenceState });
    seedStore(state, adapter);

    await dispatchAction({ type: "PassPriority" }, 0, { automated: true });

    expect(adapter.submitAction).toHaveBeenCalledOnce();
    expect(exportPersistenceState).not.toHaveBeenCalled();
    expect(useGameStore.getState().stateHistory).toHaveLength(0);
  });

  it("missing trusted export degrades to no checkpoint without failing the action", async () => {
    const state = buildGameState({ turn_number: 1, stack: [] });
    const adapter = buildEngineAdapterMock(state);
    seedStore(state, adapter);

    await dispatchAction({ type: "PassPriority" }, 0);

    expect(adapter.submitAction).toHaveBeenCalledOnce();
    expect(useGameStore.getState().stateHistory).toHaveLength(0);
    expect(useGameStore.getState().gameState).toBe(state);
  });
});
