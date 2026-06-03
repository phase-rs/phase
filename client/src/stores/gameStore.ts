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
  PlayerId,
  WaitingFor,
} from "../adapter/types";
import { MAX_UNDO_HISTORY, UNDOABLE_ACTIONS } from "../constants/game";
import { applySpellPaymentPreference } from "../game/castPaymentMode";
import { getPlayerId } from "../hooks/usePlayerId";
import { loadCheckpoints, saveGame } from "../services/gamePersistence";

/** Map a LegalActionsResult to the store fields it owns — single source of truth. */
export function legalResultState(result: LegalActionsResult): Pick<GameStoreState, "legalActions" | "autoPassRecommended" | "spellCosts" | "legalActionsByObject"> {
  return {
    legalActions: result.actions,
    autoPassRecommended: result.autoPassRecommended,
    spellCosts: result.spellCosts ?? {},
    legalActionsByObject: result.legalActionsByObject ?? {},
  };
}

// Re-export persistence API so existing imports keep working
export type { ActiveGameMeta, PersistedP2PHostSession } from "../services/gamePersistence";
export {
  saveGame,
  loadGame,
  clearGame,
  saveCheckpoints,
  loadCheckpoints,
  saveActiveGame,
  loadActiveGame,
  clearActiveGame,
  saveP2PHostSession,
  loadP2PHostSession,
  clearP2PHostSession,
} from "../services/gamePersistence";

export type GameMode = "ai" | "online" | "local" | "p2p-host" | "p2p-join" | "draft-match";

/** True for modes where the engine state is shared across the wire —
 * undo/rewind would desync from the authoritative game, so the client
 * must not build a stateHistory or expose an Undo affordance. */
export function isMultiplayerMode(mode: GameMode | null): boolean {
  return mode === "online" || mode === "p2p-host" || mode === "p2p-join" || mode === "draft-match";
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
  /**
   * Engine-grouped per-object actions keyed by source object id.
   * May include mana actions that are intentionally absent from flat
   * `legalActions`; frontend "what can I do with this card?" lookups go
   * through this map instead of inferring action availability from objects.
   */
  legalActionsByObject: Record<string, GameAction[]>;
  stateHistory: GameState[];
  turnCheckpoints: GameState[];
  /**
   * Pre-game P2P lobby fill state, populated by the `lobbyProgress` adapter
   * event and cleared when `game_setup` arrives (game starts). `null` when
   * not in a pre-game P2P lobby (i.e. during AI/online games or after the
   * game has started).
   */
  lobbyProgress: { joined: number; total: number } | null;
  /**
   * Live stack-resolution progress during a large auto-resolve / "Resolve All"
   * drain, populated per chunk by `dispatchResolveAll` and cleared when the
   * drain finishes. `null` when no resolution storm is in flight. Display-only:
   * `resolved`/`total` are engine-provided counts, never frontend-derived.
   */
  resolutionProgress: { resolved: number; total: number } | null;
  /**
   * Pure-data carrier for the starting-player d20 contest (CR 103.1): the
   * game-start `DieRolled` batch plus the engine's authoritative starting
   * player. Set once by `initGame` (null when the starter was chosen
   * explicitly). A GamePage effect consumes it to drive the dice overlay and
   * clears it via `clearStartingContest`. The store holds only data — it never
   * calls the UI store, keeping the layer boundary clean.
   */
  startingContest: { events: GameEvent[]; startingPlayer: PlayerId } | null;
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
  /**
   * Resume a P2P host game. Distinct from `resumeGame` because the
   * adapter already loaded engine state internally via
   * `wasm.resumeMultiplayerHostState` in `initialize()` — calling
   * `adapter.restoreState(savedState)` here would hit the adapter's
   * "Undo not supported in P2P games" guard.
   */
  resumeP2PHost: (gameId: string, adapter: EngineAdapter) => Promise<void>;
  dispatch: (action: GameAction) => Promise<GameEvent[]>;
  undo: () => Promise<void>;
  reset: () => void;
  setAdapter: (adapter: EngineAdapter) => void;
  setGameState: (state: GameState) => void;
  setWaitingFor: (waitingFor: WaitingFor | null) => void;
  setLegalActions: (actions: GameAction[]) => void;
  setGameMode: (mode: GameMode) => void;
  setLobbyProgress: (progress: { joined: number; total: number } | null) => void;
  setResolutionProgress: (progress: { resolved: number; total: number } | null) => void;
  /** Clear the starting-player contest after the overlay has consumed it. */
  clearStartingContest: () => void;
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
  legalActionsByObject: {},
  stateHistory: [],
  turnCheckpoints: [],
  lobbyProgress: null,
  resolutionProgress: null,
  startingContest: null,
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
      // CR 103.1: capture the starting-player d20 contest as pure data so the
      // dice overlay can animate the engine's authoritative result. Present only
      // when the engine rolled (random starter); empty for an explicit
      // play/draw choice. `current_starting_player` is the engine's pick — never
      // recomputed from the rolls on the frontend.
      const initEvents = initResult.events ?? [];
      // The engine emits a single StartingPlayerContest event (round structure +
      // winner) at the head of the game-start batch when it ran a roll-off
      // (random starter); absent for an explicit play/draw choice.
      const rolledStart = initEvents[0]?.type === "StartingPlayerContest";
      const startingContest = rolledStart
        ? {
            events: initEvents,
            startingPlayer: state.current_starting_player ?? state.active_player,
          }
        : null;
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
        startingContest,
      });
      saveGame(gameId, state);
    },

    resumeGame: async (gameId, adapter, savedState) => {
      await adapter.initialize();
      await adapter.restoreState(savedState);
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

    resumeP2PHost: async (gameId, adapter) => {
      // `adapter.initialize()` on a resumed P2PHostAdapter already
      // called `wasm.resumeMultiplayerHostState(savedState)` — the
      // engine is populated and in multiplayer mode. All we need here
      // is to pull the state out and seed the store. No stateHistory
      // (multiplayer = no undo); no checkpoints (P2P never saved them).
      await adapter.initialize();
      const state = await adapter.getState();
      const legalResult = await adapter.getLegalActions();
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
        turnCheckpoints: [],
      });
    },

    dispatch: async (action) => {
      const submittedAction = applySpellPaymentPreference(action);
      const { adapter, gameState, gameId, gameMode } = get();
      if (!adapter || !gameState) {
        throw new Error("Game not initialized");
      }

      // Save current state for undo. Three conditions must hold:
      // 1. Action type is in UNDOABLE_ACTIONS (no hidden-info leaks).
      // 2. Single-player mode — multiplayer sessions can't undo because
      //    rewinding this client's view would desync from the authoritative
      //    game state on the wire.
      // 3. Stack is empty. Checkpoints exist only at stack-empty boundaries
      //    so undo always lands the player before the most recent
      //    activation/trigger sequence, never mid-resolution.
      const shouldSaveHistory =
        UNDOABLE_ACTIONS.has(submittedAction.type) &&
        !isMultiplayerMode(gameMode) &&
        gameState.stack.length === 0;

      // `getPlayerId()` returns the local human's authenticated seat ID.
      // The engine rejects the action if this doesn't match the authorized
      // submitter — never trust the UI to route actions to the right seat.
      const result = await adapter.submitAction(submittedAction, getPlayerId());
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
      await adapter.restoreState(previous);
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

    setResolutionProgress: (progress) => {
      set({ resolutionProgress: progress });
    },

    clearStartingContest: () => {
      set({ startingContest: null });
    },
  })),
);
