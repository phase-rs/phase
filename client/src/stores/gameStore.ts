import { create } from "zustand";
import { subscribeWithSelector } from "zustand/middleware";
import type { EngineAdapter, FormatConfig, GameAction, GameEvent, GameState, WaitingFor } from "../adapter/types";
import { MAX_UNDO_HISTORY, UNDOABLE_ACTIONS } from "../constants/game";
import { ACTIVE_GAME_KEY, GAME_KEY_PREFIX } from "../constants/storage";

export interface ActiveGameMeta {
  id: string;
  mode: "ai" | "local" | "online";
  difficulty: string;
}

interface GameStoreState {
  gameId: string | null;
  gameState: GameState | null;
  events: GameEvent[];
  eventHistory: GameEvent[];
  adapter: EngineAdapter | null;
  waitingFor: WaitingFor | null;
  legalActions: GameAction[];
  stateHistory: GameState[];
}

interface GameStoreActions {
  initGame: (gameId: string, adapter: EngineAdapter, deckData?: unknown, formatConfig?: FormatConfig, playerCount?: number) => Promise<void>;
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
  adapter: null,
  waitingFor: null,
  legalActions: [],
  stateHistory: [],
};

export function saveGame(gameId: string, state: GameState): void {
  if (state.waiting_for.type === "GameOver") {
    clearGame(gameId);
    return;
  }
  try {
    localStorage.setItem(GAME_KEY_PREFIX + gameId, JSON.stringify(state));
  } catch {
    // localStorage full or unavailable — silently skip
  }
}

export function clearGame(gameId: string): void {
  localStorage.removeItem(GAME_KEY_PREFIX + gameId);
  // Clear active game if it matches
  const active = loadActiveGame();
  if (active?.id === gameId) {
    localStorage.removeItem(ACTIVE_GAME_KEY);
  }
}

export function loadGame(gameId: string): GameState | null {
  try {
    const raw = localStorage.getItem(GAME_KEY_PREFIX + gameId);
    if (!raw) return null;
    return JSON.parse(raw) as GameState;
  } catch {
    return null;
  }
}

export function saveActiveGame(meta: ActiveGameMeta): void {
  localStorage.setItem(ACTIVE_GAME_KEY, JSON.stringify(meta));
}

export function loadActiveGame(): ActiveGameMeta | null {
  try {
    const raw = localStorage.getItem(ACTIVE_GAME_KEY);
    if (!raw) return null;
    return JSON.parse(raw) as ActiveGameMeta;
  } catch {
    return null;
  }
}

export function clearActiveGame(): void {
  localStorage.removeItem(ACTIVE_GAME_KEY);
}

export const useGameStore = create<GameStore>()(
  subscribeWithSelector((set, get) => ({
    ...initialState,

    initGame: async (gameId, adapter, deckData, formatConfig, playerCount) => {
      await adapter.initialize();
      await adapter.initializeGame(deckData, formatConfig, playerCount);
      const state = await adapter.getState();
      const legalActions = await adapter.getLegalActions();
      set({
        gameId,
        adapter,
        gameState: state,
        waitingFor: state.waiting_for,
        legalActions,
        events: [],
        eventHistory: [],
        stateHistory: [],
      });
      saveGame(gameId, state);
    },

    resumeGame: async (gameId, adapter, savedState) => {
      await adapter.initialize();
      adapter.restoreState(savedState);
      const state = await adapter.getState();
      const legalActions = await adapter.getLegalActions();
      set({
        gameId,
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
      const { adapter, gameState, gameId } = get();
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

      if (gameId) saveGame(gameId, newState);

      return events;
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
