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
  ObjectAction,
  ObjectId,
  PlayerId,
  PersistedGameState,
  RewindOption,
  StuckDecisionDiagnostic,
  WaitingFor,
} from "../adapter/types";
import type { ViewerInteraction } from "../adapter/generated/interaction";
import { MAX_UNDO_HISTORY, UNDOABLE_ACTIONS } from "../constants/game";
import { applySpellPaymentPreference } from "../game/castPaymentMode";
import { getPlayerId } from "../hooks/usePlayerId";
import { loadCheckpoints, saveAuthoritativeGame } from "../services/gamePersistence";
import { resetStackThroughput } from "../utils/stackThroughput";

/** Map a LegalActionsResult to the store fields it owns — single source of truth. */
export function legalResultState(result: LegalActionsResult): Pick<GameStoreState, "legalActions" | "autoPassRecommended" | "endContinuousEffectOffers" | "manaPaymentShortcutActions" | "spellCosts" | "legalActionsByObject" | "stuckDiagnostic" | "viewerInteraction"> {
  return {
    legalActions: result.actions,
    autoPassRecommended: result.autoPassRecommended,
    endContinuousEffectOffers: result.endContinuousEffectOffers ?? [],
    manaPaymentShortcutActions: result.manaPaymentShortcutActions ?? [],
    spellCosts: result.spellCosts ?? {},
    legalActionsByObject: result.legalActionsByObject ?? {},
    stuckDiagnostic: result.stuckDiagnostic ?? null,
    viewerInteraction: result.viewerInteraction ?? null,
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

/** Where the authoritative engine state lives. `"wire"` means some process
 *  other than this client owns it, so this client must never rewind: a local
 *  rewind desyncs from the authority on the next exchange. */
export type EngineAuthority = "client" | "wire";

/** Whether humans on OTHER clients share this game. `"solo"` covers hot-seat
 *  `local` too: two humans at one screen share one client and one state, so
 *  nothing they do can desync a peer or leak hidden info across a wire. */
export type TableCompany = "solo" | "remote-humans";

/** Where this client's OWN seat number comes from.
 *
 * `"seat-zero"` — a solo game. There is one local human and the engine seats
 * them at 0 by construction; nothing on a wire can say otherwise, so a stale
 * `activePlayerId` left behind by an earlier online game must not be read.
 *
 * `"wire-assigned"` — somebody else hands this client its seat: a server
 * (`playerIdentity` from `WebSocketAdapter`), a P2P host (`game_setup`'s
 * `assignedPlayerId`), or the pod that paired this match
 * (`setupDraftMatchAvatars`). `multiplayerStore.activePlayerId` carries it.
 *
 * `"no-seat"` — a spectator holds no seat at all. The two seat resolvers in
 * `usePlayerId.ts` deliberately answer this case differently; see the comment
 * there for the contract `HudBadges.tsx` depends on. */
export type SeatSource = "seat-zero" | "wire-assigned" | "no-seat";

interface GameModeTraits {
  readonly authority: EngineAuthority;
  readonly company: TableCompany;
  readonly seat: SeatSource;
}

/**
 * The two questions the old `isMultiplayerMode` answered with one bit, split
 * and answered separately for every mode. Exhaustive by construction: a new
 * `GameMode` fails to compile until it declares both axes.
 *
 * This table is the census. Never widen a predicate with an `|| mode === "..."`
 * at a call site — that is how the conflation this file exists to undo comes
 * back, one call site at a time.
 *
 * `spectate` is `remote-humans` by the *game* it observes, not by the observer.
 */
export const GAME_MODE_TRAITS: Record<GameMode, GameModeTraits> = {
  "ai": { authority: "client", company: "solo", seat: "seat-zero" },
  "local": { authority: "client", company: "solo", seat: "seat-zero" },
  "native-ai": { authority: "wire", company: "solo", seat: "seat-zero" },
  "online": { authority: "wire", company: "remote-humans", seat: "wire-assigned" },
  "p2p-host": { authority: "wire", company: "remote-humans", seat: "seat-zero" },
  "p2p-join": { authority: "wire", company: "remote-humans", seat: "wire-assigned" },
  "draft-match": { authority: "wire", company: "remote-humans", seat: "wire-assigned" },
  "spectate": { authority: "wire", company: "remote-humans", seat: "no-seat" },
};

/** True when the authoritative engine state lives off this client — i.e. the
 *  authority is REMOTE. Such a client must not rewind its own view
 *  (`stateHistory`/`undo`); see `WebSocketAdapter.restoreState`, which rejects
 *  it at the transport. This is deliberately NOT "is this multiplayer":
 *  desktop solo-vs-AI (`native-ai`) is a wire-authoritative game with no other
 *  humans in it, and every one of this predicate's seven call sites wants the
 *  authority question, not the company one. */
export function isAuthorityRemote(mode: GameMode | null): boolean {
  return mode !== null && GAME_MODE_TRAITS[mode].authority === "wire";
}

/**
 * True when humans on other clients share this game. Local-only affordances
 * are safe when false — there is nobody else's game to disturb and no hidden
 * info to leak across a wire.
 *
 * **First and only production consumer: `GamePage`'s `takebackAudience`.**
 * `GamePage` passes `hasRemoteHumans(storeGameMode) ? "table" : "solo"` to
 * `GameMenu`, which is what makes the desktop solo-vs-AI menu entry read "Undo
 * Last Action" rather than "Request Takeback". That gate is the company
 * question, not the authority one: `native-ai` is `authority: "wire"` (the
 * sidecar owns the state) but has no other human at the table, and asking
 * `isAuthorityRemote` there would label a solo undo as a request to somebody.
 * That mislabelling is exactly what the merged predicate got wrong, and
 * splitting the two axes is the fix.
 *
 * Every *other* gate that reads `gameMode` still asks the authority question
 * above; keep it that way. Reach for this one only when the question really is
 * "are other humans watching?", and prefer calling it over appending an `||` to
 * a mode list — the string union is frozen taxonomy, and a hand-rolled list
 * silently misses the next mode added.
 *
 * Do not repurpose it for transport questions: `canRestoreCheckpoints`
 * (`DebugPanel.tsx`) looks like a company gate but is really "does this
 * adapter implement `restoreState`", and `native-ai`'s does not.
 */
export function hasRemoteHumans(mode: GameMode | null): boolean {
  return mode !== null && GAME_MODE_TRAITS[mode].company === "remote-humans";
}

/**
 * The seat axis of the census: where this client's own seat number comes from.
 *
 * Read this instead of testing `gameMode` against a list at the call site. The
 * list form is what seated a pod-draft guest at 0: `"draft-match"` joined the
 * union long after `usePlayerId`'s list was written, and nothing made the two
 * meet. A mode added to `GAME_MODE_TRAITS` cannot compile without declaring
 * its seat source, so it can never again default into somebody else's chair.
 *
 * `null` — no game yet — is `"seat-zero"`: nothing has assigned this client
 * anything.
 */
export function seatSource(mode: GameMode | null): SeatSource {
  return mode === null ? "seat-zero" : GAME_MODE_TRAITS[mode].seat;
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
  /** Ordered engine-authored CR 116.2c offers, rendered unchanged. */
  endContinuousEffectOffers: NonNullable<LegalActionsResult["endContinuousEffectOffers"]>;
  /** Exact engine-authored actions dispatched by the tap-all-mana shortcut. */
  manaPaymentShortcutActions: GameAction[];
  /** Effective mana costs for castable spells, keyed by object_id string. */
  spellCosts: Record<string, ManaCost>;
  /**
   * Engine-grouped per-object actions keyed by source object id.
   * May include mana actions that are intentionally absent from flat
   * `legalActions`; frontend "what can I do with this card?" lookups go
   * through this map instead of inferring action availability from objects.
   */
  legalActionsByObject: Record<string, ObjectAction[]>;
  /**
   * Engine-owned non-fatal progress-wedge diagnostic (an engine anomaly, not a
   * rules outcome) — present only when the current decision is wedged (no legal
   * action for any authorized submitter). `null` in normal play. Display-only
   * (drives `StuckDecisionToast`).
   */
  stuckDiagnostic: StuckDecisionDiagnostic | null;
  /** Viewer-scoped interaction projection from the same engine snapshot. */
  viewerInteraction: ViewerInteraction | null;
  stateHistory: GameState[];
  turnCheckpoints: GameState[];
  /**
   * Server-published turn boundaries offered as rollback targets. Mirrors the
   * server exactly — never appended to client-side, never derived. Empty on
   * every transport that does not publish them (which is all of them except a
   * `SingleUser` phase-server sidecar).
   */
  rewindTargets: RewindOption[];
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
  | "endContinuousEffectOffers"
  | "manaPaymentShortcutActions"
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
  /**
   * Resume a native-engine solo (AI) game. Like `resumeP2PHost` the game is
   * server-authoritative — the local phase-server holds the state and the
   * reconnecting adapter's `initialize()` yields it — so there is no local
   * snapshot to `restoreState` and no undo history to rebuild.
   */
  resumeNativeSolo: (gameId: string, adapter: EngineAdapter) => Promise<void>;
  dispatch: (action: GameAction) => Promise<GameEvent[]>;
  undo: () => Promise<void>;
  /**
   * Replace the server-published rollback targets. Only `dispatch.ts` calls
   * this, and only from inside its generation gate — a superseded remote update
   * must not clobber the list with a stale one.
   */
  setRewindTargets: (targets: RewindOption[]) => void;
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

/**
 * Seed the store from a server-authoritative adapter whose `initialize()` has
 * already produced the current game state (resumed P2P host, or a reconnected
 * native solo game). These games have no client-side undo history and no local
 * checkpoints — the server owns the state — so `stateHistory`/`turnCheckpoints`
 * stay empty. Shared by `resumeP2PHost` and `resumeNativeSolo`.
 */
async function seedResumedServerGame(
  get: () => GameStore,
  gameId: string,
  adapter: EngineAdapter,
): Promise<void> {
  // Reset stack-pacing throughput — resuming may load a different game than the
  // one just played; stale churn must not carry across.
  resetStackThroughput();
  await adapter.initialize();
  // Fetched after `initialize()` restored/attached the engine state, so the
  // snapshot is newest-by-construction and always passes the commit gate.
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
      rewindTargets: [],
    },
  });
}

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
  endContinuousEffectOffers: [],
  manaPaymentShortcutActions: [],
  spellCosts: {},
  legalActionsByObject: {},
  stuckDiagnostic: null,
  viewerInteraction: null,
  stateHistory: [],
  turnCheckpoints: [],
  rewindTargets: [],
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
      // Network-backed adapters can publish the initial authoritative snapshot
      // from inside `initializeGame`. Bind the transport before that happens so
      // the shared remote-update path never commits a visible game state whose
      // action dispatcher has no adapter yet.
      set({ adapter });
      let initResult;
      try {
        initResult = await adapter.initializeGame(
          deckData,
          formatConfig,
          playerCount,
          matchConfig,
          firstPlayer,
        );
      } catch (error) {
        // A failed initialization must not leave a transport that never
        // produced a playable game attached to the store.
        if (get().adapter === adapter) set({ adapter: null });
        throw error;
      }
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
          rewindTargets: [],
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
          rewindTargets: [],
        },
      });
    },

    resumeP2PHost: async (gameId, adapter) => {
      // `adapter.initialize()` on a resumed P2PHostAdapter already called
      // `wasm.resumeMultiplayerHostState(savedState)` — the engine is populated
      // and in multiplayer mode — so the shared helper just pulls the state out
      // and seeds the store. No stateHistory (multiplayer = no undo); no
      // checkpoints (P2P never saved them).
      await seedResumedServerGame(get, gameId, adapter);
    },

    resumeNativeSolo: async (gameId, adapter) => {
      // The reconnecting native adapter's `initialize()` sends a reconnect frame
      // and resolves once the phase-server replays the current GameStarted
      // state; the shared helper then seeds the store from that authority.
      await seedResumedServerGame(get, gameId, adapter);
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
        !isAuthorityRemote(gameMode) &&
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
      if (isAuthorityRemote(gameMode)) return;
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

    setRewindTargets: (targets) => {
      set({ rewindTargets: targets });
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
