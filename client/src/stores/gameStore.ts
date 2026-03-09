import { create } from "zustand";
import { subscribeWithSelector } from "zustand/middleware";
import type { EngineAdapter, GameAction, GameEvent, GameState, WaitingFor } from "../adapter/types";
import { MAX_UNDO_HISTORY, UNDOABLE_ACTIONS } from "../constants/game";

interface GameStoreState {
  gameState: GameState | null;
  events: GameEvent[];
  eventHistory: GameEvent[];
  adapter: EngineAdapter | null;
  waitingFor: WaitingFor | null;
  legalActions: GameAction[];
  stateHistory: GameState[];
}

interface GameStoreActions {
  initGame: (adapter: EngineAdapter, deckData?: unknown) => Promise<void>;
  dispatch: (action: GameAction) => Promise<GameEvent[]>;
  undo: () => void;
  reset: () => void;
}

export type GameStore = GameStoreState & GameStoreActions;

const initialState: GameStoreState = {
  gameState: null,
  events: [],
  eventHistory: [],
  adapter: null,
  waitingFor: null,
  legalActions: [],
  stateHistory: [],
};

export const useGameStore = create<GameStore>()(
  subscribeWithSelector((set, get) => ({
    ...initialState,

    initGame: async (adapter, deckData) => {
      await adapter.initialize();
      await adapter.initializeGame(deckData);
      const state = await adapter.getState();
      const legalActions = await adapter.getLegalActions();
      set({
        adapter,
        gameState: state,
        waitingFor: state.waiting_for,
        legalActions,
        events: [],
        eventHistory: [],
        stateHistory: [],
      });
    },

    dispatch: async (action) => {
      const { adapter, gameState } = get();
      if (!adapter || !gameState) {
        throw new Error("Game not initialized");
      }

      // Save current state for undo (only for unrevealed-information actions)
      const shouldSaveHistory = UNDOABLE_ACTIONS.has(action.type);

      const events = await adapter.submitAction(action);
      const newState = await adapter.getState();
      const legalActions = await adapter.getLegalActions();

      set((prev) => {
        const newHistory = shouldSaveHistory
          ? [...prev.stateHistory, gameState].slice(-MAX_UNDO_HISTORY)
          : prev.stateHistory;

        return {
          gameState: newState,
          events,
          eventHistory: [...prev.eventHistory, ...events].slice(-1000),
          waitingFor: newState.waiting_for,
          legalActions,
          stateHistory: newHistory,
        };
      });

      return events;
    },

    undo: () => {
      const { stateHistory, adapter } = get();
      if (stateHistory.length === 0 || !adapter) return;

      const previous = stateHistory[stateHistory.length - 1];

      // Sync WASM engine state with the restored client state
      adapter.restoreState(previous);

      set({
        gameState: previous,
        waitingFor: previous.waiting_for,
        events: [],
        stateHistory: stateHistory.slice(0, -1),
      });
    },

    reset: () => {
      const { adapter } = get();
      if (adapter) {
        adapter.dispose();
      }
      set(initialState);
    },
  })),
);
