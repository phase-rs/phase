import { create } from "zustand";
import { subscribeWithSelector } from "zustand/middleware";
import type {
  EngineAdapter,
  FormatConfig,
  GameAction,
  GameEvent,
  GameLogEntry,
  GameState,
  LegalActionsResult,
  ManaCost,
  MatchConfig,
  WaitingFor,
} from "../adapter/types";
import { MAX_UNDO_HISTORY, UNDOABLE_ACTIONS } from "../constants/game";
import { getPlayerId } from "../hooks/usePlayerId";
import { loadCheckpoints, saveGame } from "../services/gamePersistence";

/** Map a LegalActionsResult to the store fields it owns — single source of truth. */
export function legalResultState(result: LegalActionsResult): Pick<GameStoreState, "legalActions" | "autoPassRecommended" | "spellCosts"> {
  return {
    legalActions: result.actions,
    autoPassRecommended: result.autoPassRecommended,
    spellCosts: result.spellCosts ?? {},
  };
}

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

export type GameMode = "ai" | "online" | "local" | "p2p-host" | "p2p-join";

/** True for modes where the engine state is shared across the wire —
 * undo/rewind would desync from the authoritative game, so the client
 * must not build a stateHistory or expose an Undo affordance. */
export function isMultiplayerMode(mode: GameMode | null): boolean {
  return mode === "online" || mode === "p2p-host" || mode === "p2p-join";
}

interface GameStoreState {
  gameId: string | null;
  gameMode: GameMode | null;
  gameState: GameState | null;
  events: GameEvent[];
  eventHistory: GameEvent[];
  logHistory: GameLogEntry[];
  nextLogSeq: number;
  adapter: EngineAdapter | null;
  waitingFor: WaitingFor | null;
  legalActions: GameAction[];
  autoPassRecommended: boolean;
  /** Effective mana costs for castable spells, keyed by object_id string. */
  spellCosts: Record<string, ManaCost>;
  stateHistory: GameState[];
  turnCheckpoints: GameState[];
  /**
   * Pre-game P2P lobby fill state, populated by the `lobbyProgress` adapter
   * event and cleared when `game_setup` arrives (game starts). `null` when
   * not in a pre-game P2P lobby (i.e. during AI/online games or after the
   * game has started).
   */
  lobbyProgress: { joined: number; total: number } | null;
}

interface GameStoreActions {
  initGame: (
    gameId: string,
    adapter: EngineAdapter,
    deckData?: unknown,
    formatConfig?: FormatConfig,
    playerCount?: number,
    matchConfig?: MatchConfig,
    firstPlayer?: number,
  ) => Promise<void>;
  resumeGame: (gameId: string, adapter: EngineAdapter, savedState: GameState) => Promise<void>;
  dispatch: (action: GameAction) => Promise<GameEvent[]>;
  undo: () => Promise<void>;
  reset: () => void;
  setAdapter: (adapter: EngineAdapter) => void;
  setGameState: (state: GameState) => void;
  setWaitingFor: (waitingFor: WaitingFor | null) => void;
  setLegalActions: (actions: GameAction[]) => void;
  setGameMode: (mode: GameMode) => void;
  setLobbyProgress: (progress: { joined: number; total: number } | null) => void;
}

export type GameStore = GameStoreState & GameStoreActions;

const initialState: GameStoreState = {
  gameId: null,
  gameMode: null,
  gameState: null,
  events: [],
  eventHistory: [],
  logHistory: [],
  nextLogSeq: 0,
  adapter: null,
  waitingFor: null,
  legalActions: [],
  autoPassRecommended: false,
  spellCosts: {},
  stateHistory: [],
  turnCheckpoints: [],
  lobbyProgress: null,
};

export const useGameStore = create<GameStore>()(
  subscribeWithSelector((set, get) => ({
    ...initialState,

    initGame: async (gameId, adapter, deckData, formatConfig, playerCount, matchConfig, firstPlayer) => {
      await adapter.initialize();
      const initResult = await adapter.initializeGame(deckData, formatConfig, playerCount, matchConfig, firstPlayer);
      const state = await adapter.getState();
      const legalResult = await adapter.getLegalActions();
      const initLogEntries = (initResult.log_entries ?? []).map((entry, i) => ({
        ...entry,
        seq: i,
      }));
      set({
        gameId,
        adapter,
        gameState: state,
        waitingFor: state.waiting_for,
        ...legalResultState(legalResult),
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
      const legalResult = await adapter.getLegalActions();
      const savedCheckpoints = await loadCheckpoints(gameId);
      set({
        gameId,
        adapter,
        gameState: state,
        waitingFor: state.waiting_for,
        ...legalResultState(legalResult),
        events: [],
        eventHistory: [],
        logHistory: [],
        nextLogSeq: 0,
        stateHistory: [],
        turnCheckpoints: savedCheckpoints,
      });
    },

    dispatch: async (action) => {
      const { adapter, gameState, gameId, gameMode } = get();
      if (!adapter || !gameState) {
        throw new Error("Game not initialized");
      }

      // Save current state for undo (only for unrevealed-information actions,
      // and only in single-player — multiplayer sessions can't undo because
      // rewinding this client's view would desync from the authoritative
      // game state on the wire).
      const shouldSaveHistory =
        UNDOABLE_ACTIONS.has(action.type) && !isMultiplayerMode(gameMode);

      // `getPlayerId()` returns the local human's authenticated seat ID.
      // The engine rejects the action if this doesn't match the authorized
      // submitter — never trust the UI to route actions to the right seat.
      const result = await adapter.submitAction(action, getPlayerId());
      const newState = await adapter.getState();
      const legalResult = await adapter.getLegalActions();

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
          ...legalResultState(legalResult),
          stateHistory: newHistory,
        };
      });

      if (gameId) saveGame(gameId, newState);

      return result.events;
    },

    undo: async () => {
      const { stateHistory, adapter, gameMode } = get();
      if (isMultiplayerMode(gameMode)) return;
      if (stateHistory.length === 0 || !adapter) return;

      const previous = stateHistory[stateHistory.length - 1];

      // Sync WASM engine state with the restored client state
      adapter.restoreState(previous);
      const legalResult = await adapter.getLegalActions();

      set({
        gameState: previous,
        waitingFor: previous.waiting_for,
        ...legalResultState(legalResult),
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

    setGameMode: (mode) => {
      set({ gameMode: mode });
    },

    setLobbyProgress: (progress) => {
      set({ lobbyProgress: progress });
    },
  })),
);
