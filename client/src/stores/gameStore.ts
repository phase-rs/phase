import { create } from "zustand";
import { subscribeWithSelector } from "zustand/middleware";
import type {
  EngineAdapter,
  FormatConfig,
  GameAction,
  GameEvent,
  GameLogEntry,
  GameState,
  MatchConfig,
  WaitingFor,
} from "../adapter/types";
import { MAX_UNDO_HISTORY, UNDOABLE_ACTIONS } from "../constants/game";
import { loadCheckpoints, saveGame } from "../services/gamePersistence";

// Re-export persistence API so existing imports keep working
export type { ActiveGameMeta } from "../services/gamePersistence";
export {
  saveGame,
  loadGame,
  clearGame,
  saveCheckpoints,
  loadCheckpoints,
  saveActiveGame,
  loadActiveGame,
  clearActiveGame,
} from "../services/gamePersistence";

interface GameStoreState {
  gameId: string | null;
  gameState: GameState | null;
  events: GameEvent[];
  eventHistory: GameEvent[];
  logHistory: GameLogEntry[];
  nextLogSeq: number;
  adapter: EngineAdapter | null;
  waitingFor: WaitingFor | null;
  legalActions: GameAction[];
  stateHistory: GameState[];
  turnCheckpoints: GameState[];
}

interface GameStoreActions {
  initGame: (
    gameId: string,
    adapter: EngineAdapter,
    deckData?: unknown,
    formatConfig?: FormatConfig,
    playerCount?: number,
    matchConfig?: MatchConfig,
  ) => Promise<void>;
  resumeGame: (gameId: string, adapter: EngineAdapter, savedState: GameState) => Promise<void>;
  dispatch: (action: GameAction) => Promise<GameEvent[]>;
  undo: () => Promise<void>;
  reset: () => void;
  setAdapter: (adapter: EngineAdapter) => void;
  setGameState: (state: GameState) => void;
  setWaitingFor: (waitingFor: WaitingFor | null) => void;
  setLegalActions: (actions: GameAction[]) => void;
}

export type GameStore = GameStoreState & GameStoreActions;

const initialState: GameStoreState = {
  gameId: null,
  gameState: null,
  events: [],
  eventHistory: [],
  logHistory: [],
  nextLogSeq: 0,
  adapter: null,
  waitingFor: null,
  legalActions: [],
  stateHistory: [],
  turnCheckpoints: [],
};

export const useGameStore = create<GameStore>()(
  subscribeWithSelector((set, get) => ({
    ...initialState,

    initGame: async (gameId, adapter, deckData, formatConfig, playerCount, matchConfig) => {
      await adapter.initialize();
      const initResult = await adapter.initializeGame(deckData, formatConfig, playerCount, matchConfig);
      const state = await adapter.getState();
      const legalActions = await adapter.getLegalActions();
      const initLogEntries = (initResult.log_entries ?? []).map((entry, i) => ({
        ...entry,
        seq: i,
      }));
      set({
        gameId,
        adapter,
        gameState: state,
        waitingFor: state.waiting_for,
        legalActions,
        events: [],
        eventHistory: [],
        logHistory: initLogEntries,
        nextLogSeq: initLogEntries.length,
        stateHistory: [],
        turnCheckpoints: [],
      });
      saveGame(gameId, state);
    },

    resumeGame: async (gameId, adapter, savedState) => {
      await adapter.initialize();
      adapter.restoreState(savedState);
      const state = await adapter.getState();
      const legalActions = await adapter.getLegalActions();
      const savedCheckpoints = await loadCheckpoints(gameId);
      set({
        gameId,
        adapter,
        gameState: state,
        waitingFor: state.waiting_for,
        legalActions,
        events: [],
        eventHistory: [],
        logHistory: [],
        nextLogSeq: 0,
        stateHistory: [],
        turnCheckpoints: savedCheckpoints,
      });
    },

    dispatch: async (action) => {
      const { adapter, gameState, gameId } = get();
      if (!adapter || !gameState) {
        throw new Error("Game not initialized");
      }

      // Save current state for undo (only for unrevealed-information actions)
      const shouldSaveHistory = UNDOABLE_ACTIONS.has(action.type);

      const result = await adapter.submitAction(action);
      const newState = await adapter.getState();
      const legalActions = await adapter.getLegalActions();

      set((prev) => {
        const newHistory = shouldSaveHistory
          ? [...prev.stateHistory, gameState].slice(-MAX_UNDO_HISTORY)
          : prev.stateHistory;

        // Assign monotonic sequence numbers to new log entries
        let seq = prev.nextLogSeq;
        const newLogEntries = (result.log_entries ?? []).map((entry) => ({
          ...entry,
          seq: seq++,
        }));

        return {
          gameState: newState,
          events: result.events,
          eventHistory: [...prev.eventHistory, ...result.events].slice(-1000),
          logHistory: [...prev.logHistory, ...newLogEntries].slice(-2000),
          nextLogSeq: seq,
          waitingFor: newState.waiting_for,
          legalActions,
          stateHistory: newHistory,
        };
      });

      if (gameId) saveGame(gameId, newState);

      return result.events;
    },

    undo: async () => {
      const { stateHistory, adapter } = get();
      if (stateHistory.length === 0 || !adapter) return;

      const previous = stateHistory[stateHistory.length - 1];

      // Sync WASM engine state with the restored client state
      adapter.restoreState(previous);
      const legalActions = await adapter.getLegalActions();

      set({
        gameState: previous,
        waitingFor: previous.waiting_for,
        legalActions,
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

    setAdapter: (adapter) => {
      set({ adapter });
    },

    setGameState: (state) => {
      set({ gameState: state });
    },

    setWaitingFor: (waitingFor) => {
      set({ waitingFor });
    },

    setLegalActions: (actions) => {
      set({ legalActions: actions });
    },
  })),
);
