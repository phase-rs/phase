import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory.ts";
import {
  buildGameObjectWithCoreTypes,
  buildObjectMap,
} from "../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../test/factories/gameStateFactory.ts";
import { useDebugHandReveal } from "../useDebugHandReveal.ts";

const AI_CARD = 22;

/**
 * A fake local engine: `get_game_state` projects the AI's hand card by the
 * current disclosure request, and installing a game resets that request — the
 * WASM bridge's contract.
 */
function fakeEngine() {
  let revealRequested = false;
  const project = (): GameState => {
    const card = buildGameObjectWithCoreTypes(["Creature"], {
      id: AI_CARD,
      card_id: AI_CARD,
      owner: 1,
      controller: 1,
      zone: "Hand",
      name: revealRequested ? "AI Secret Card" : "Hidden Card",
      display_visible_to_viewer: revealRequested,
    });
    return buildGameState({
      players: buildPlayers([0, { id: 1, hand: [AI_CARD] }]),
      objects: buildObjectMap(card),
      seat_order: [0, 1],
    });
  };
  const adapter = buildEngineAdapterMock(project(), {
    getState: vi.fn(async () => project()),
  });
  const setDebugHandReveal = vi.fn(async (enabled: boolean) => {
    revealRequested = enabled;
  });
  Object.assign(adapter, { setDebugHandReveal });
  /** Install a replacement game on the same adapter (a rematch). */
  const installReplacementGame = () => {
    revealRequested = false;
    useGameStore.setState((store) => ({
      gameState: project(),
      gameSessionGeneration: store.gameSessionGeneration + 1,
    }));
  };
  return { adapter, project, setDebugHandReveal, installReplacementGame };
}

function aiCard() {
  return useGameStore.getState().gameState?.objects[AI_CARD];
}

describe("useDebugHandReveal", () => {
  let engine: ReturnType<typeof fakeEngine>;

  beforeEach(() => {
    engine = fakeEngine();
    useGameStore.setState({
      adapter: engine.adapter,
      gameState: engine.project(),
      gameSessionGeneration: useGameStore.getState().gameSessionGeneration + 1,
      lastCommittedSeq: 0,
    });
  });

  afterEach(() => {
    cleanup();
    useGameStore.setState({ adapter: null, gameState: null });
  });

  it("sends nothing while the toggle is off", () => {
    renderHook(() => useDebugHandReveal(false));

    expect(engine.setDebugHandReveal).not.toHaveBeenCalled();
    expect(aiCard()?.display_visible_to_viewer).toBe(false);
  });

  it("requests the reveal and publishes the engine's revealed snapshot", async () => {
    renderHook(() => useDebugHandReveal(true));

    await waitFor(() => expect(aiCard()?.name).toBe("AI Secret Card"));
    expect(engine.setDebugHandReveal).toHaveBeenCalledWith(true);
    expect(aiCard()?.display_visible_to_viewer).toBe(true);
  });

  it("withdraws the reveal and publishes the hidden snapshot when toggled off", async () => {
    const { rerender } = renderHook(({ on }) => useDebugHandReveal(on), {
      initialProps: { on: true },
    });
    await waitFor(() => expect(aiCard()?.display_visible_to_viewer).toBe(true));

    rerender({ on: false });

    await waitFor(() => expect(aiCard()?.name).toBe("Hidden Card"));
    expect(engine.setDebugHandReveal).toHaveBeenLastCalledWith(false);
    expect(aiCard()?.display_visible_to_viewer).toBe(false);
  });

  it("re-requests the reveal for a replacement game installed on the same adapter", async () => {
    renderHook(() => useDebugHandReveal(true));
    await waitFor(() => expect(aiCard()?.display_visible_to_viewer).toBe(true));
    engine.setDebugHandReveal.mockClear();

    act(() => engine.installReplacementGame());
    expect(aiCard()?.display_visible_to_viewer).toBe(false);

    await waitFor(() => expect(aiCard()?.name).toBe("AI Secret Card"));
    expect(engine.setDebugHandReveal).toHaveBeenLastCalledWith(true);
  });
});
