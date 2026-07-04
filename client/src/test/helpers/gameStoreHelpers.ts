import { act } from "react";
import { vi } from "vitest";

import type { GameAction, GameState, WaitingFor } from "../../adapter/types";
import { useGameStore, type GameStore } from "../../stores/gameStore";
import { buildGameState } from "../factories/gameStateFactory";

type GameStoreTestState = Partial<GameStore> & {
  dispatch?: GameStore["dispatch"];
  gameState?: GameState;
  legalActions?: GameAction[];
  waitingFor?: WaitingFor | null;
};

export function setGameStoreForTest({
  gameState = buildGameState(),
  waitingFor = gameState.waiting_for,
  dispatch = vi.fn().mockResolvedValue([]),
  legalActions,
  ...overrides
}: GameStoreTestState = {}) {
  act(() => {
    useGameStore.setState({
      gameState,
      waitingFor,
      dispatch,
      ...(legalActions === undefined ? {} : { legalActions }),
      ...overrides,
    });
  });

  return { dispatch, gameState, waitingFor };
}
