import { create } from "zustand";
import { subscribeWithSelector } from "zustand/middleware";
import type { EngineAdapter, GameAction, GameEvent, GameState, WaitingFor } from "../adapter/types";

/** Action types that don't reveal hidden information and are safe to undo. */
const UNDOABLE_ACTIONS = new Set([
  "PassPriority",
  "DeclareAttackers",
  "DeclareBlockers",
  "ActivateAbility",
]);

/** Maximum number of undo history entries. */
const MAX_UNDO_HISTORY = 5;

interface GameStoreState {
  gameState: GameState | null;
  events: GameEvent[];
  adapter: EngineAdapter | null;
  waitingFor: WaitingFor | null;
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
  adapter: null,
  waitingFor: null,
  stateHistory: [],
};

export const useGameStore = create<GameStore>()(
  subscribeWithSelector((set, get) => ({
    ...initialState,

    initGame: async (adapter, deckData) => {
      await adapter.initialize();
      await adapter.initializeGame(deckData);
      const state = await adapter.getState();
      set({
        adapter,
        gameState: state,
        waitingFor: state.waiting_for,
        events: [],
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

      set((prev) => {
        const newHistory = shouldSaveHistory
          ? [...prev.stateHistory, gameState].slice(-MAX_UNDO_HISTORY)
          : prev.stateHistory;

        return {
          gameState: newState,
          events,
          waitingFor: newState.waiting_for,
          stateHistory: newHistory,
        };
      });

      return events;
    },

    undo: () => {
      const { stateHistory } = get();
      if (stateHistory.length === 0) return;

      const previous = stateHistory[stateHistory.length - 1];
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
