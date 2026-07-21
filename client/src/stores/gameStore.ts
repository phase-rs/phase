import { create } from "zustand";
import { subscribeWithSelector } from "zustand/middleware";
import type {
  EngineAdapter,
  EngineSnapshot,
  FormatConfig,
  GameAction,
  GameEvent,
  GameLogEntry,
  GameState,
  LegalActionsResult,
  ManaCost,
  MatchConfig,
  ObjectId,
  PlayerId,
  PersistedGameState,
  StuckDecisionDiagnostic,
  WaitingFor,
} from "../adapter/types";
import { MAX_UNDO_HISTORY, UNDOABLE_ACTIONS } from "../constants/game";
import { applySpellPaymentPreference } from "../game/castPaymentMode";
import { getPlayerId } from "../hooks/usePlayerId";
import { loadCheckpoints, saveAuthoritativeGame } from "../services/gamePersistence";
import { resetStackThroughput } from "../utils/stackThroughput";

/** Map a LegalActionsResult to the store fields it owns — single source of truth. */
export function legalResultState(result: LegalActionsResult): Pick<GameStoreState, "legalActions" | "autoPassRecommended" | "spellCosts" | "legalActionsByObject" | "stuckDiagnostic"> {
  return {
    legalActions: result.actions,
    autoPassRecommended: result.autoPassRecommended,
    spellCosts: result.spellCosts ?? {},
    legalActionsByObject: result.legalActionsByObject ?? {},
    stuckDiagnostic: result.stuckDiagnostic ?? null,
  };
}

// Re-export persistence API so existing imports keep working
export type { ActiveGameMeta, PersistedP2PHostSession } from "../services/gamePersistence";
export {
  saveGame,
  saveAuthoritativeGame,
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

export type GameMode =
  | "ai"
  | "native-ai"
  | "online"
  | "local"
  | "p2p-host"
  | "p2p-join"
  | "draft-match"
  | "spectate";

/** True for modes where the engine state is shared across the wire —
 * undo/rewind would desync from the authoritative game, so the client
 * must not build a stateHistory or expose an Undo affordance. */
export function isMultiplayerMode(mode: GameMode | null): boolean {
  return (
    mode === "native-ai"
    || mode === "online"
    || mode === "p2p-host"
    || mode === "p2p-join"
    || mode === "draft-match"
    || mode === "spectate"
  );
}

interface GameStoreState {
  gameId: string | null;
  gameMode: GameMode | null;
  /** Transport selected for the current solo-AI game. F.5 telemetry reads this
   * alongside `nativeEngineFallbackReason`; neither field drives game rules. */
  engineMode: "native" | "wasm" | null;
  nativeEngineFallbackReason: string | null;
  gameState: GameState | null;
  events: GameEvent[];
  eventHistory: GameEvent[];
  logHistory: GameLogEntry[];
  nextLogSeq: number;
  adapter: EngineAdapter | null;
  /** Monotonically unique local game lifecycle identity. Unlike gameId, it
   * changes for a fresh init/resume/reset even when the adapter and id are
   * reused. Transient: never persisted or restored from engine snapshots. */
  gameSessionGeneration: number;
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
  /**
   * Engine-owned non-fatal progress-wedge diagnostic (an engine anomaly, not a
   * rules outcome) — present only when the current decision is wedged (no legal
   * action for any authorized submitter). `null` in normal play. Display-only
   * (drives `StuckDecisionToast`).
   */
  stuckDiagnostic: StuckDecisionDiagnostic | null;
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
   * True while the worker is draining a Resolve All batch. Separate from
   * `resolutionProgress` because small drains may finish without showing the
   * storm progress overlay, but controls should still be disabled.
   */
  isResolvingAll: boolean;
  /**
   * Pure-data carrier for the starting-player d20 contest (CR 103.1): the
   * game-start `DieRolled` batch plus the engine's authoritative starting
   * player. Set once by `initGame` (null when the starter was chosen
   * explicitly). A GamePage effect consumes it to drive the dice overlay and
   * clears it via `clearStartingContest`. The store holds only data — it never
   * calls the UI store, keeping the layer boundary clean.
   */
  startingContest: { events: GameEvent[]; startingPlayer: PlayerId } | null;
  /**
   * PlayerIds bound to AI controllers this game. Client-owned lobby/session
   * config (NOT game-state derivation): set at game init from the resolved AI
   * seat bindings and cleared on `reset`. Empty for human-only games (online /
   * p2p). Consumed by telemetry `game_end` to classify `winner_kind`.
   */
  aiSeatIds: PlayerId[];
  /**
   * `EngineSnapshot.seq` of the most recently committed engine pair — the gate
   * `commitEngineSnapshot` uses to drop commits derived from an older engine
   * version than one already applied. Transient (never persisted); returns to 0
   * with the rest of `initialState` on `reset`.
   */
  lastCommittedSeq: number;
  /**
   * Monotonic local commit counter. Unlike `lastCommittedSeq`, this advances
   * for an accepted equal-sequence snapshot too, so asynchronous display
   * previews can prove they still describe the current engine snapshot.
   */
  engineCommitEpoch: number;
  /**
   * Engine-returned mana sources for the spell currently being dragged. This
   * display state is cleared with every accepted engine snapshot.
   */
  manaPaymentPreviewSourceIds: ObjectId[];
}

/**
 * Fields written exclusively by `commitEngineSnapshot` from the snapshot's own
 * contents. `extraState` structurally EXCLUDES them: were they writable there,
 * a caller could smuggle an ungated pair field past the revision gate and
 * reintroduce exactly the mixed-epoch commit this authority exists to prevent.
 * `lastCommittedSeq` (the gate counter itself) is excluded for the same reason.
 */
type CommitExtraState = Partial<Omit<GameStoreState,
  | "gameState"
  | "waitingFor"
  | "legalActions"
  | "autoPassRecommended"
  | "spellCosts"
  | "legalActionsByObject"
  | "stuckDiagnostic"
  | "lastCommittedSeq"
  | "engineCommitEpoch"
  | "manaPaymentPreviewSourceIds">>;

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
  resumeGame: (gameId: string, adapter: EngineAdapter, savedState: PersistedGameState) => Promise<void>;
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
  /**
   * THE single writer of the live-game engine pair (`gameState`, `waitingFor`,
   * and every `legalResultState(...)` field). Every live-game commit — local
   * dispatch, remote update, batch resolve, init, resume, undo, restore —
   * routes through here.
   *
   * Revision gate: the pair is applied iff `snapshot.seq >= lastCommittedSeq`,
   * so a commit derived from an OLDER engine version can never clobber a newer
   * one already applied. (Equal seq arises only from two reads of the same
   * cached wire snapshot, whose pairs are byte-identical, so `>=` is idempotent
   * and lets a remote update and a local read of that snapshot coexist.)
   * Returns false when the pair was dropped as stale.
   *
   * Events, log entries, and undo checkpoints are applied ALWAYS, even for a
   * dropped pair: history is ordered by arrival, not by engine epoch, and a
   * checkpoint is a pre-action state that stays valid whichever pair wins.
   *
   * Known residue (documented, not fixed here): because history applies
   * unconditionally, a leftover cross-match commit can append game-1 entries
   * into game-2's histories after its pair is correctly dropped. Strictly less
   * wrong than the pre-fix behavior, where the whole stale pair clobbered.
   *
   * Documented exemptions from this authority (all write outside a live game,
   * or are immediately superseded by a newest-by-construction commit):
   * `replayStore` timeline scrubbing, the GameOver-only `waitingFor` writes in
   * `GamePage`, `sessionCleanup`'s session-boundary prompt clear, and the
   * teardown clears in `GameProvider`/`disposeMatchAdapter`.
   */
  commitEngineSnapshot: (
    snapshot: EngineSnapshot,
    opts?: {
      /** Replaces `events`; appended to `eventHistory`. Applied even when the pair is dropped. */
      events?: GameEvent[];
      /** Seq-stamped and appended to `logHistory`. Applied even when the pair is dropped. */
      logEntries?: GameLogEntry[];
      /** Undo checkpoints. Applied even when the pair is dropped. */
      stateHistory?: GameState[];
      /**
       * Site-specific fields applied in the SAME `set()` — but only when the
       * pair commit is accepted, and after the base commit + history handling,
       * so init/resume/restore sites can atomically reset or seed history
       * fields alongside their pair.
       */
      extraState?: CommitExtraState;
    },
  ) => boolean;
  setGameMode: (mode: GameMode) => void;
  setEngineMode: (mode: "native" | "wasm" | null, fallbackReason?: string | null) => void;
  setLobbyProgress: (progress: { joined: number; total: number } | null) => void;
  setResolutionProgress: (progress: { resolved: number; total: number } | null) => void;
  setIsResolvingAll: (isResolvingAll: boolean) => void;
  setManaPaymentPreviewSourceIds: (sourceIds: ObjectId[]) => void;
  clearManaPaymentPreview: () => void;
  /** Clear the starting-player contest after the overlay has consumed it. */
  clearStartingContest: () => void;
}

let latestGameSessionGeneration = 0;

export function nextGameSessionGeneration(): number {
  latestGameSessionGeneration += 1;
  return latestGameSessionGeneration;
}

export type GameStore = GameStoreState & GameStoreActions;

const initialState: GameStoreState = {
  gameId: null,
  gameMode: null,
  engineMode: null,
  nativeEngineFallbackReason: null,
  gameState: null,
  events: [],
  eventHistory: [],
  logHistory: [],
  nextLogSeq: 0,
  adapter: null,
  gameSessionGeneration: nextGameSessionGeneration(),
  waitingFor: null,
  legalActions: [],
  autoPassRecommended: false,
  spellCosts: {},
  legalActionsByObject: {},
  stuckDiagnostic: null,
  stateHistory: [],
  turnCheckpoints: [],
  lobbyProgress: null,
  resolutionProgress: null,
  isResolvingAll: false,
  startingContest: null,
  aiSeatIds: [],
  lastCommittedSeq: 0,
  engineCommitEpoch: 0,
  manaPaymentPreviewSourceIds: [],
};

export const useGameStore = create<GameStore>()(
  subscribeWithSelector((set, get) => ({
    ...initialState,

    commitEngineSnapshot: (snapshot, opts) => {
      // Decide the gate BEFORE `set`, so the updater stays a pure reducer.
      // Safe: `get()` → `set()` runs synchronously with no `await` between, so
      // no other commit can land in the window.
      const accepted = snapshot.seq >= get().lastCommittedSeq;

      set((prev) => {
        // Seq-stamp incoming log entries against the CURRENT counter.
        let nextLogSeq = prev.nextLogSeq;
        const stampedLogEntries = (opts?.logEntries ?? []).map((entry) => ({
          ...entry,
          seq: nextLogSeq++,
        }));

        return {
          // 1. The engine pair — gated.
          ...(accepted
            ? {
                gameState: snapshot.state,
                waitingFor: snapshot.state.waiting_for,
                ...legalResultState(snapshot.legalResult),
                lastCommittedSeq: snapshot.seq,
                engineCommitEpoch: prev.engineCommitEpoch + 1,
                manaPaymentPreviewSourceIds: [],
              }
            : {}),
          // 2. History — ordered by arrival, so applied unconditionally.
          ...(opts?.events
            ? {
                events: opts.events,
                eventHistory: [...prev.eventHistory, ...opts.events].slice(-1000),
              }
            : {}),
          ...(opts?.logEntries
            ? {
                logHistory: [...prev.logHistory, ...stampedLogEntries].slice(-2000),
                nextLogSeq,
              }
            : {}),
          ...(opts?.stateHistory ? { stateHistory: opts.stateHistory } : {}),
          // 3. Site-specific fields last, so an init/resume/restore reset wins
          //    over the history append above.
          ...(accepted ? opts?.extraState : undefined),
        };
      });
      return accepted;
    },

    initGame: async (gameId, adapter, deckData, formatConfig, playerCount, matchConfig, firstPlayer) => {
      // Clear the display-only stack-pacing tracker so a fast-churning end to a
      // prior game can't bleed stale resolution rate into this game's opening
      // pacing (rematch started within the throughput window).
      resetStackThroughput();
      await adapter.initialize();
      const initResult = await adapter.initializeGame(deckData, formatConfig, playerCount, matchConfig, firstPlayer);
      // Fetched AFTER the engine is initialized, so this snapshot is
      // newest-by-construction under the global counter — it always passes the
      // gate, and it drops any leftover in-flight commit from a prior match.
      const snapshot = await adapter.getSnapshot();
      const state = snapshot.state;
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
      get().commitEngineSnapshot(snapshot, {
        extraState: {
          gameId,
          adapter,
          gameSessionGeneration: nextGameSessionGeneration(),
          events: [],
          eventHistory: [],
          logHistory: initLogEntries,
          nextLogSeq: initLogEntries.length,
          stateHistory: [],
          turnCheckpoints: [],
          startingContest,
        },
      });
      void saveAuthoritativeGame(gameId, adapter, state);
    },

    resumeGame: async (gameId, adapter, savedState) => {
      // Reset stack-pacing throughput — resuming may load a different game than
      // the one just played; stale churn must not carry across.
      resetStackThroughput();
      await adapter.initialize();
      await adapter.restoreState(savedState);
      // Post-restore fetch — newest-by-construction, so it always passes the gate.
      const snapshot = await adapter.getSnapshot();
      const savedCheckpoints = await loadCheckpoints(gameId);
      get().commitEngineSnapshot(snapshot, {
        extraState: {
          gameId,
          adapter,
          gameSessionGeneration: nextGameSessionGeneration(),
          events: [],
          eventHistory: [],
          logHistory: [],
          nextLogSeq: 0,
          stateHistory: [],
          turnCheckpoints: savedCheckpoints,
        },
      });
    },

    resumeP2PHost: async (gameId, adapter) => {
      // Reset stack-pacing throughput on entry to this game context.
      resetStackThroughput();
      // `adapter.initialize()` on a resumed P2PHostAdapter already
      // called `wasm.resumeMultiplayerHostState(savedState)` — the
      // engine is populated and in multiplayer mode. All we need here
      // is to pull the state out and seed the store. No stateHistory
      // (multiplayer = no undo); no checkpoints (P2P never saved them).
      await adapter.initialize();
      // Fetched after that `initialize()` (which is what restored the engine, per
      // the note above), so the snapshot is newest-by-construction.
      const snapshot = await adapter.getSnapshot();
      get().commitEngineSnapshot(snapshot, {
        extraState: {
          gameId,
          adapter,
          gameSessionGeneration: nextGameSessionGeneration(),
          events: [],
          eventHistory: [],
          logHistory: [],
          nextLogSeq: 0,
          stateHistory: [],
          turnCheckpoints: [],
        },
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
      // ONE atomic pair — a separate getState()/getLegalActions() pair could
      // straddle an engine advance and commit a mismatched state/actions pair.
      const snapshot = await adapter.getSnapshot();

      // Read-then-commit with no `await` between, so no other commit interleaves.
      const stateHistory = shouldSaveHistory
        ? [...get().stateHistory, gameState].slice(-MAX_UNDO_HISTORY)
        : undefined;
      get().commitEngineSnapshot(snapshot, {
        events: result.events,
        logEntries: result.log_entries ?? [],
        stateHistory,
      });

      if (gameId) void saveAuthoritativeGame(gameId, adapter, snapshot.state);

      return result.events;
    },

    undo: async () => {
      const { stateHistory, adapter, gameMode } = get();
      if (isMultiplayerMode(gameMode)) return;
      if (stateHistory.length === 0 || !adapter) return;

      const previous = stateHistory[stateHistory.length - 1];

      // Sync WASM engine state with the restored client state
      await adapter.restoreState(previous);
      // Commit the snapshot's OWN state, not `previous`: post-restore the engine
      // is the source of truth, and taking both halves from one snapshot is what
      // keeps the pair coherent. Newest-by-construction, so it passes the gate.
      const snapshot = await adapter.getSnapshot();

      get().commitEngineSnapshot(snapshot, {
        extraState: {
          events: [],
          stateHistory: stateHistory.slice(0, -1),
        },
      });
    },

    reset: () => {
      const { adapter } = get();
      if (adapter) {
        adapter.dispose();
      }
      set({ ...initialState, gameSessionGeneration: nextGameSessionGeneration() });
    },

    setAdapter: (adapter) => {
      set({ adapter });
    },

    setGameMode: (mode) => {
      set({ gameMode: mode });
    },

    setEngineMode: (mode, fallbackReason = null) => {
      set({ engineMode: mode, nativeEngineFallbackReason: fallbackReason });
    },

    setLobbyProgress: (progress) => {
      set({ lobbyProgress: progress });
    },

    setResolutionProgress: (progress) => {
      set({ resolutionProgress: progress });
    },

    setIsResolvingAll: (isResolvingAll) => {
      set({ isResolvingAll });
    },

    setManaPaymentPreviewSourceIds: (sourceIds) => {
      set({ manaPaymentPreviewSourceIds: sourceIds });
    },

    clearManaPaymentPreview: () => {
      set({ manaPaymentPreviewSourceIds: [] });
    },

    clearStartingContest: () => {
      set({ startingContest: null });
    },
  })),
);
