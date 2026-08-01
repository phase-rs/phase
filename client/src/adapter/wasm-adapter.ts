import type {
  AiActionProposal,
  AiProposalSubmission,
  BatchResolveResult,
  EngineAdapter,
  EngineSnapshot,
  FormatConfig,
  GameAction,
  GameState,
  LegalActionsResult,
  MatchConfig,
  ObjectId,
  PersistedGameState,
  PlayerId,
  SubmitResult,
  ViewerSnapshot,
} from "./types";
import type { InteractionSubmission } from "./generated/interaction";
import { AdapterError, AdapterErrorCode, isStaleRejectionMessage, isStateLostMessage, nextSnapshotSeq } from "./types";
import type { BracketDeckRequest, BracketEstimate } from "../types/bracketEstimate";
import { isBracketEstimate } from "../types/bracketEstimate";
import { EngineWorkerClient } from "./engine-worker-client";
import { AiWorkerPool } from "./ai-worker-pool";
import type { AiCardDataMode, AiPoolCardDbPlan } from "./card-db-subset";
import {
  applyAiPoolCardDbPlan,
  DEFAULT_AI_CARD_DATA_MODE,
  resolveAiPoolCardDbPlan,
} from "./card-db-subset";

function isMemoryConstrainedDevice(): boolean {
  if (typeof navigator === "undefined") return false;
  const isIOS = /iP(hone|od|ad)/.test(navigator.userAgent)
    || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
  return isIOS || (/Android/.test(navigator.userAgent) && /Mobile/.test(navigator.userAgent));
}

/**
 * Flatten the `ClientGameState { state, derived }` wire envelope produced
 * by the engine's WASM getters into the store-side `GameState` shape with
 * `derived` attached as an optional field. When the runtime returns a
 * plain `GameState` (older WASM build, post-state-loss sentinel), the
 * wrapped shape is absent and we pass through untouched.
 *
 * See `crates/engine/src/game/derived_views.rs`.
 */
export function unwrapClientGameState(raw: unknown): GameState {
  if (raw != null && typeof raw === "object" && "state" in raw) {
    const wrapped = raw as { state: GameState; derived?: GameState["derived"] };
    return { ...wrapped.state, derived: wrapped.derived ?? wrapped.state.derived };
  }
  return raw as GameState;
}

/**
 * Classify an unknown error thrown by the engine worker or main-thread
 * fallback. If the Rust sentinel prefix is present, escalate to an
 * `AdapterError` — STATE_LOST when the cell was simply emptied, or
 * ENGINE_PANIC when the panic hook captured a message (which means the
 * loss was caused by a Rust panic and retrying will re-panic).
 *
 * Async because the panic drain (`take_last_panic_message`) is a worker
 * round-trip; the choice between STATE_LOST and ENGINE_PANIC depends on
 * whether a panic was observed during this call.
 */
async function classifyEngineErrorAsync(
  err: unknown,
  takePanic: () => Promise<string | null>,
): Promise<Error> {
  // Returns (rather than throws) the error to surface so call sites can
  // write `throw await classifyEngineErrorAsync(...)`. TypeScript doesn't
  // always narrow control flow through an awaited `Promise<never>`, so
  // making the throw explicit keeps the surrounding methods type-clean.
  const message = err instanceof Error ? err.message : String(err);
  // Actor-authorization rejection (stale action after a priority/turn shift).
  // Typed so dispatch can treat it as a benign no-op rather than a crash.
  if (isStaleRejectionMessage(message)) {
    return new AdapterError(AdapterErrorCode.STALE_ACTION, message, false);
  }
  if (isStateLostMessage(message)) {
    let panic: string | null = null;
    try {
      // Drain BEFORE deciding — `take_last_panic_message` is consuming, so a
      // panic that occurred during this call is observed exactly once.
      panic = await takePanic();
    } catch {
      // takePanic itself failed (worker dead, etc.) — fall through to
      // STATE_LOST. The recovery layer's existing rehydrate-then-retry
      // path is the safe default when we can't prove a panic occurred.
    }
    if (panic) {
      return new AdapterError(AdapterErrorCode.ENGINE_PANIC, message, false, panic);
    }
    return new AdapterError(AdapterErrorCode.STATE_LOST, message, true);
  }
  return err instanceof Error ? err : new Error(message);
}

/**
 * Module-level singleton for AI/local games.
 *
 * Keeping the WASM worker alive across game sessions preserves V8's TurboFan-compiled
 * code. The first WASM instantiation runs on V8's Liftoff (unoptimized) baseline compiler
 * while TurboFan optimizes in the background. Terminating the worker discards this work;
 * reusing it means AI computation runs at full speed from the second game onward.
 * The card database and AI worker pool are also preserved.
 */
let sharedAdapter: WasmAdapter | null = null;

/** Get or create the shared WasmAdapter singleton for AI/local games. */
export function getSharedAdapter(): WasmAdapter {
  if (!sharedAdapter) sharedAdapter = new WasmAdapter();
  return sharedAdapter;
}

/**
 * WASM-backed implementation of EngineAdapter.
 *
 * Delegates all engine operations to a Web Worker that owns its own WASM instance.
 * The main thread never loads WASM — keeping the UI thread free from engine computation.
 *
 * Falls back to direct main-thread WASM calls if Worker creation fails
 * (e.g., restrictive CSP, very old browser).
 */
export class WasmAdapter implements EngineAdapter {
  private initialized = false;
  cardDbLoaded = false;

  // Worker-based engine (primary path)
  private engine: EngineWorkerClient | null = null;

  // Score-only workers are an optional VeryHard optimization. They never own
  // proposals or action submission authority.
  private aiPool: AiWorkerPool | null = null;
  private aiPoolPromise: Promise<AiWorkerPool | null> | null = null;
  private aiPoolGeneration = 0;
  private aiPoolFailed = false;
  private aiPoolUnboundedGame = false;
  private aiCardDataMode: AiCardDataMode = DEFAULT_AI_CARD_DATA_MODE;

  // Fallback: direct WASM on main thread (only used if Worker fails)
  private fallback: MainThreadFallback | null = null;

  // In-flight init dedupe. The `initialized` flag only flips *after* the worker
  // handshake resolves, so without this a second concurrent `initialize()`
  // (e.g. menu card-DB warm racing an un-gated Resume click) would pass the
  // flag check and spawn a second EngineWorkerClient, orphaning the first
  // worker's ~90 MB instance. Concurrent callers share one promise.
  private initPromise: Promise<void> | null = null;
  private lifecycleGeneration = 0;

  async initialize(): Promise<void> {
    if (this.initialized) return;
    if (this.initPromise) return this.initPromise;
    const generation = this.lifecycleGeneration;
    const pending = (async () => {
      let candidateEngine: EngineWorkerClient | null = null;
      try {
        candidateEngine = new EngineWorkerClient();
        await candidateEngine.initialize();
        if (this.lifecycleGeneration !== generation) {
          candidateEngine.dispose();
          return;
        }
        this.engine = candidateEngine;
      } catch (error) {
        candidateEngine?.dispose();
        if (this.lifecycleGeneration !== generation) return;
        // Worker creation or initialization failed — fall back to main-thread WASM
        console.warn(
          "Web Worker initialization failed, falling back to main-thread WASM",
          error,
        );
        const candidateFallback = await createMainThreadFallback();
        if (this.lifecycleGeneration !== generation) return;
        this.fallback = candidateFallback;
      }
      this.initialized = true;
    })();
    // If init rejects (worker AND fallback both fail), clear the cached promise
    // so a later call retries instead of replaying a stuck rejection forever.
    // `pending` is returned so the current caller still sees the error; only
    // future callers get a fresh attempt — matching the pre-dedupe semantics.
    pending.catch(() => {
      if (this.initPromise === pending) this.initPromise = null;
    });
    this.initPromise = pending;
    return pending;
  }

  // In-flight card-DB load dedupe. `cardDbLoaded` only flips true *after* the
  // ~3-5s fetch+parse completes, so without this every caller that arrives
  // during that window (menu warm racing bracket/compat/feed prewarm, all of
  // which call ensureCardDb directly and bypass cardDataStore's warmInFlight)
  // sees the flag still false and queues its own `loadCardDbFromUrl` on the
  // worker. The worker drains its queue serially, re-fetching and re-parsing
  // the full ~90 MB DB for each — a staggered burst of redundant loads.
  // Concurrent callers now share one load; mirrors `initPromise` above.
  private cardDbPromise: Promise<void> | null = null;

  private ensureCardDb(): Promise<void> {
    if (this.cardDbLoaded) return Promise.resolve();
    if (this.cardDbPromise) return this.cardDbPromise;
    const pending = (async () => {
      try {
        if (this.engine) {
          const count = await this.engine.loadCardDbFromUrl();
          console.log(`Card database loaded in worker: ${count} cards`);
        } else if (this.fallback) {
          const count = await this.fallback.ensureCardDatabase();
          console.log(`Card database loaded: ${count} cards`);
        }
        this.cardDbLoaded = true;
        if (this.engine && this.aiPool && !this.aiPool.isCardDbLoaded) {
          await this.ensureAiPool();
        }
      } catch (err) {
        console.warn("Failed to load card database:", err);
      }
    })();
    // Clear the in-flight ref once settled so a *failed* load (cardDbLoaded
    // still false) can be retried by a later caller. A successful load
    // short-circuits on the `cardDbLoaded` latch above and never re-enters.
    this.cardDbPromise = pending.finally(() => {
      this.cardDbPromise = null;
    });
    return this.cardDbPromise;
  }

  /** Drain the captured panic, defaulting to `null` for the main-thread
   *  fallback (no separate worker to query) or when the worker has died.
   *
   *  Bounded by a 250ms timer because a STATE_LOST sentinel can mean the
   *  worker itself crashed/restarted — in which case the round-trip never
   *  resolves and would hang every error path indefinitely. A live worker
   *  responds in <10ms (the read is a synchronous thread-local take); the
   *  timer only fires for dead workers, where treating the panic as
   *  "uncaptured" correctly falls back to the legacy STATE_LOST flow.
   */
  private takePanic = (): Promise<string | null> => {
    if (!this.engine) return Promise.resolve(null);
    const drain = this.engine.takeLastPanic().catch(() => null);
    const timeout = new Promise<null>((resolve) => setTimeout(() => resolve(null), 250));
    return Promise.race([drain, timeout]);
  };

  async submitAction(action: GameAction, actor: PlayerId): Promise<SubmitResult> {
    this.assertInitialized();
    if (action.type === "Debug" && action.data.type === "CreateCard") {
      await this.ensureCardDb();
    }
    try {
      if (this.engine) return await this.engine.submitAction(actor, action);
      return await this.fallback!.submitAction(action, actor);
    } catch (err) {
      throw await classifyEngineErrorAsync(err, this.takePanic);
    }
  }

  async submitInteraction(
    submission: InteractionSubmission,
    actor: PlayerId,
  ): Promise<SubmitResult> {
    this.assertInitialized();
    try {
      if (this.engine) return await this.engine.submitInteraction(actor, submission);
      return await this.fallback!.submitInteraction(submission, actor);
    } catch (err) {
      throw await classifyEngineErrorAsync(err, this.takePanic);
    }
  }

  async previewManaPayment(action: GameAction, actor: PlayerId): Promise<ObjectId[]> {
    this.assertInitialized();
    try {
      if (this.engine) return await this.engine.previewManaPayment(actor, action);
      return await this.fallback!.previewManaPayment(action, actor);
    } catch (err) {
      throw await classifyEngineErrorAsync(err, this.takePanic);
    }
  }

  async getState(): Promise<GameState> {
    this.assertInitialized();
    try {
      // WASM `get_game_state` now returns ClientGameState { state, derived }.
      // Flatten to the store's GameState shape by attaching `derived` as an
      // optional field on the state object. Components that don't consume
      // derived (the vast majority) see no change.
      const wrapped = this.engine
        ? await this.engine.getState()
        : await this.fallback!.getState();
      return unwrapClientGameState(wrapped);
    } catch (err) {
      throw await classifyEngineErrorAsync(err, this.takePanic);
    }
  }

  async getFilteredState(viewerId: number): Promise<GameState> {
    this.assertInitialized();
    try {
      const wrapped = this.engine
        ? await this.engine.getFilteredState(viewerId)
        : await this.fallback!.getFilteredState(viewerId);
      return unwrapClientGameState(wrapped);
    } catch (err) {
      throw await classifyEngineErrorAsync(err, this.takePanic);
    }
  }

  async getLegalActions(): Promise<LegalActionsResult> {
    this.assertInitialized();
    try {
      if (this.engine) return await this.engine.getLegalActions();
      return await this.fallback!.getLegalActions();
    } catch (err) {
      throw await classifyEngineErrorAsync(err, this.takePanic);
    }
  }

  async getLegalActionsForViewer(viewerId: number): Promise<LegalActionsResult> {
    this.assertInitialized();
    try {
      if (this.engine) return await this.engine.getLegalActionsForViewer(viewerId);
      return await this.fallback!.getLegalActionsForViewer(viewerId);
    } catch (err) {
      throw await classifyEngineErrorAsync(err, this.takePanic);
    }
  }

  /**
   * Atomic state + legal-actions pair, stamped when the response ARRIVES.
   *
   * Worker responses post in worker-processing order and awaiting callers
   * resume in resolution order, so stamping here reproduces engine order even
   * with concurrent callers. The `state` half gets the same
   * `unwrapClientGameState` envelope flatten `getState` applies — without it a
   * raw `{ state, derived }` envelope would reach the store and silently break
   * every `derived` consumer.
   */
  async getSnapshot(): Promise<EngineSnapshot> {
    this.assertInitialized();
    try {
      const raw = this.engine
        ? await this.engine.getSnapshot()
        : await this.fallback!.getSnapshot();
      return {
        state: unwrapClientGameState(raw.state),
        legalResult: raw.legalResult,
        seq: nextSnapshotSeq(),
      };
    } catch (err) {
      throw await classifyEngineErrorAsync(err, this.takePanic);
    }
  }

  async getViewerSnapshot(viewerId: number): Promise<ViewerSnapshot> {
    this.assertInitialized();
    try {
      const wrapped = this.engine
        ? await this.engine.getViewerSnapshot(viewerId)
        : await this.fallback!.getViewerSnapshot(viewerId);
      // The `state` field needs the same client-side unwrap as `getFilteredState`
      // to normalize serde-wasm-bindgen oddities (Map-as-Object conversion etc).
      return { ...wrapped, state: unwrapClientGameState(wrapped.state) };
    } catch (err) {
      throw await classifyEngineErrorAsync(err, this.takePanic);
    }
  }

  async getAiActionProposal(
    difficulty: string,
    playerId: number,
  ): Promise<AiActionProposal | null> {
    this.assertInitialized();
    try {
      if (difficulty === "VeryHard" && this.engine) {
        try {
          // A snapshot can become stale while scoring. That is safe: the main
          // worker rebinds every score against a newly-issued contract below.
          const state = await this.engine.getState();
          if (state.waiting_for.type === "Priority") {
            const pool = await this.ensureAiPool();
            if (pool) {
              const scores = await pool.getAiScoredCandidates(
                await this.engine.exportState(),
                difficulty,
                playerId,
              );
              if (scores?.length) {
                const proposal = await this.engine.getAiActionProposalFromScores(
                  JSON.stringify(scores),
                  difficulty,
                  playerId,
                  Date.now(),
                );
                if (proposal) return proposal;
              }
            }
          }
        } catch (error) {
          if (error instanceof Error && isStateLostMessage(error.message)) throw error;
          console.warn("AI worker pool failed; using authoritative single worker", error);
        }
      }
      if (this.engine) return await this.engine.getAiActionProposal(difficulty, playerId);
      return await this.fallback!.getAiActionProposal(difficulty, playerId);
    } catch (err) {
      throw await classifyEngineErrorAsync(err, this.takePanic);
    }
  }

  async submitAiActionProposal(
    proposal: AiActionProposal,
  ): Promise<AiProposalSubmission> {
    this.assertInitialized();
    try {
      if (this.engine) return await this.engine.submitAiActionProposal(proposal);
      return await this.fallback!.submitAiActionProposal(proposal);
    } catch (err) {
      throw await classifyEngineErrorAsync(err, this.takePanic);
    }
  }

  private trackAiPoolWork(pending: Promise<AiWorkerPool | null>): Promise<AiWorkerPool | null> {
    this.aiPoolPromise = pending;
    const clear = () => {
      if (this.aiPoolPromise === pending) this.aiPoolPromise = null;
    };
    void pending.then(clear, clear);
    return pending;
  }

  private async reloadAiPoolGameDb(
    engine: EngineWorkerClient,
    pool: AiWorkerPool,
    generation: number,
  ): Promise<AiWorkerPool | null> {
    try {
      const plan = await resolveAiPoolCardDbPlan(this.aiCardDataMode, engine);
      if (generation !== this.aiPoolGeneration || pool !== this.aiPool) return null;
      if (plan.kind === "unbounded") {
        pool.dispose();
        this.aiPool = null;
        this.aiPoolUnboundedGame = true;
        return null;
      }
      await applyAiPoolCardDbPlan(plan, pool);
      if (generation !== this.aiPoolGeneration || pool !== this.aiPool) {
        // A reset may have started a new game while this old subset loaded.
        // Never let that result become usable by the next decision.
        pool.invalidateCardDb();
        return null;
      }
      return pool;
    } catch (error) {
      if (generation === this.aiPoolGeneration && pool === this.aiPool) {
        console.warn("Failed to load bounded card DB into AI pool", error);
      }
      return null;
    }
  }

  private ensureAiPool(): Promise<AiWorkerPool | null> {
    if (!this.engine || !this.cardDbLoaded || this.aiPoolUnboundedGame || isMemoryConstrainedDevice()) {
      return Promise.resolve(null);
    }
    if (this.aiPoolPromise) return this.aiPoolPromise;
    if (this.aiPool) {
      return this.aiPool.isCardDbLoaded
        ? Promise.resolve(this.aiPool)
        : this.trackAiPoolWork(this.reloadAiPoolGameDb(this.engine, this.aiPool, this.aiPoolGeneration));
    }
    if (this.aiPoolFailed) return Promise.resolve(null);
    return this.trackAiPoolWork(this.createAiPool(this.aiPoolGeneration));
  }

  private async createAiPool(generation: number): Promise<AiWorkerPool | null> {
    let candidate: AiWorkerPool | null = null;
    try {
      const plan: AiPoolCardDbPlan = await resolveAiPoolCardDbPlan(this.aiCardDataMode, this.engine!);
      if (generation !== this.aiPoolGeneration) return null;
      if (plan.kind === "unbounded") {
        this.aiPoolUnboundedGame = true;
        return null;
      }
      const workers = Math.max(2, Math.min((navigator.hardwareConcurrency ?? 0) - 1, 4));
      candidate = new AiWorkerPool(workers);
      await candidate.initialize();
      await applyAiPoolCardDbPlan(plan, candidate);
      if (generation !== this.aiPoolGeneration) {
        candidate.dispose();
        return null;
      }
      this.aiPool = candidate;
      return candidate;
    } catch (error) {
      candidate?.dispose();
      if (generation === this.aiPoolGeneration) {
        this.aiPool = null;
        this.aiPoolFailed = true;
      }
      console.warn("AI worker pool unavailable; using authoritative single worker", error);
      return null;
    }
  }

  async resolveAll(
    requester: number,
    aiSeats: { playerId: number; difficulty: string }[],
    maxResolutions: number = 0,
  ): Promise<BatchResolveResult> {
    this.assertInitialized();
    if (this.engine) {
      return this.engine.resolveAll(requester, aiSeats, maxResolutions);
    }
    throw new Error("resolveAll requires worker-based engine");
  }

  private async requireCardDbForRestore(): Promise<void> {
    await this.ensureCardDb();
    // Soft-failed ensureCardDb leaves cardDbLoaded false and skips
    // rehydrate_game_from_card_db — restored CardName NamedChoices then have
    // empty legal actions and softlock the AI (#6393). Refuse DB-less restore
    // / P2P host resume the same way warmCardDatabase surfaces load failure.
    if (!this.cardDbLoaded) {
      throw new Error("Card database failed to load");
    }
  }

  async restoreState(state: PersistedGameState): Promise<void> {
    this.assertInitialized();
    await this.requireCardDbForRestore();
    const json = JSON.stringify(state);
    if (this.engine) await this.engine.restoreState(json);
    else await this.fallback!.restoreState(json);
  }

  /**
   * Export the engine-authored trusted persistence envelope. The local store
   * may retain this opaque JSON, but only the engine decodes its private route
   * runtime on restore.
   */
  async exportPersistenceState(): Promise<string> {
    this.assertInitialized();
    if (this.engine) return this.engine.exportState();
    return this.fallback!.exportState();
  }

  /**
   * Toggle the engine's multiplayer enforcement flag. When enabled, the
   * Rust side refuses `restore_game_state` with a descriptive error —
   * defense against any caller trying to rewind a multiplayer game.
   * Called by multiplayer adapters (P2P host/guest) after WASM init.
   */
  async setMultiplayerMode(enabled: boolean): Promise<void> {
    this.assertInitialized();
    if (this.engine) {
      await this.engine.setMultiplayerMode(enabled);
    } else {
      this.fallback!.setMultiplayerMode(enabled);
    }
  }

  async applySeatMutation(stateJson: string, mutationJson: string): Promise<unknown> {
    this.assertInitialized();
    await this.ensureCardDb();
    if (this.engine) {
      return this.engine.applySeatMutation(stateJson, mutationJson);
    }
    return this.fallback!.applySeatMutation(stateJson, mutationJson);
  }

  async projectSeatView(stateJson: string): Promise<unknown> {
    this.assertInitialized();
    if (this.engine) {
      return this.engine.projectSeatView(stateJson);
    }
    return this.fallback!.projectSeatView(stateJson);
  }

  /**
   * Resume a P2P host session from a persisted `GameState`. Stamps a fresh
   * RNG seed (so continued play diverges from the pre-save sequence) and
   * atomically flips the engine's multiplayer flag. The engine must be
   * in its initial (post-`initialize()`) state — a prior game must be
   * cleared via `clear_game_state` first.
   *
   * Distinct from `restoreState` (undo semantics, deterministic re-seed).
   * Mirrors `server-core::GameSession::from_persisted`.
   */
  async resumeMultiplayerHostState(state: PersistedGameState): Promise<void> {
    this.assertInitialized();
    // Same CARD_DB requirement as restoreState — resume rehydrates abilities
    // only when the DB is loaded (engine-wasm resume_multiplayer_host_state).
    await this.requireCardDbForRestore();
    const json = JSON.stringify(state);
    if (this.engine) await this.engine.resumeMultiplayerHostState(json);
    else await this.fallback!.resumeMultiplayerHostState(json);
  }

  /** Clear the WASM game state without terminating the worker. */
  async resetGameState(): Promise<void> {
    this.aiPoolGeneration += 1;
    this.aiPoolPromise = null;
    this.aiPoolFailed = false;
    this.aiPoolUnboundedGame = false;
    if (this.aiCardDataMode !== "full") this.aiPool?.invalidateCardDb();
    if (this.engine) {
      await this.engine.resetGame();
    }
  }

  async estimateBracket(deck: BracketDeckRequest): Promise<BracketEstimate | null> {
    this.assertInitialized();
    if (this.engine) {
      return this.engine.estimateBracketForDeck(deck);
    }
    return this.fallback!.estimateBracketForDeck(deck);
  }

  /**
   * Eagerly load the card database into the shared worker so later compat
   * checks and game init are instant. Public entry point for the menu/page
   * warm. Unlike the best-effort `ensureCardDb` (used by Debug CreateCard),
   * this surfaces failure so `cardDataStore` can show an `error` status — the
   * underlying cause is still logged inside `ensureCardDb`.
   */
  async warmCardDatabase(): Promise<void> {
    await this.initialize();
    await this.requireCardDbForRestore();
  }

  /**
   * Run a stateless deck-compatibility check against the shared worker's card
   * database. Replaces the former dedicated compatibility worker — one engine
   * instance now serves both compat checks and gameplay (a CARD_DB read only,
   * no game-state mutation), mirroring the existing `estimateBracket` query.
   */
  async checkDeckCompatibility(request: unknown): Promise<unknown> {
    await this.initialize();
    await this.ensureCardDb();
    if (this.engine) {
      return this.engine.evaluateDeckCompatibility(request);
    }
    return this.fallback!.evaluateDeckCompatibility(request);
  }

  /**
   * Display-only card queries share the authoritative engine worker and its
   * resident card database. Keeping these off the main-thread runtime avoids a
   * second WASM module + corpus allocation when card UI mounts during gameplay.
   */
  async getCardFaceData(cardName: string): Promise<unknown> {
    await this.initialize();
    await this.ensureCardDb();
    if (this.engine) return this.engine.getCardFaceData(cardName);
    return this.fallback!.getCardFaceData(cardName);
  }

  async getCardParseDetails(cardName: string): Promise<unknown> {
    await this.initialize();
    await this.ensureCardDb();
    if (this.engine) return this.engine.getCardParseDetails(cardName);
    return this.fallback!.getCardParseDetails(cardName);
  }

  async getCardRulings(cardName: string): Promise<unknown> {
    await this.initialize();
    await this.ensureCardDb();
    if (this.engine) return this.engine.getCardRulings(cardName);
    return this.fallback!.getCardRulings(cardName);
  }

  dispose(): void {
    this.lifecycleGeneration += 1;
    this.aiPoolGeneration += 1;
    // Clear the singleton reference so getSharedAdapter() creates a fresh
    // instance if called after dispose (e.g., error recovery code paths).
    if (sharedAdapter === this) sharedAdapter = null;
    this.engine?.dispose();
    this.engine = null;
    this.aiPool?.dispose();
    this.aiPool = null;
    this.aiPoolPromise = null;
    this.aiPoolFailed = false;
    this.aiPoolUnboundedGame = false;
    this.fallback = null;
    this.initialized = false;
    this.initPromise = null;
    this.cardDbLoaded = false;
    this.cardDbPromise = null;
  }

  async ping(): Promise<string> {
    this.assertInitialized();
    if (this.engine) {
      return this.engine.ping();
    }
    return this.fallback!.ping();
  }

  async initializeGame(
    deckData?: unknown,
    formatConfig?: FormatConfig,
    playerCount?: number,
    matchConfig?: MatchConfig,
    firstPlayer?: number,
  ): Promise<SubmitResult> {
    this.assertInitialized();
    if (deckData) {
      await this.ensureCardDb();
    }
    const seed = Math.floor(Math.random() * Number.MAX_SAFE_INTEGER);
    if (this.engine) {
      return this.engine.initializeGame(
        deckData ?? null,
        seed,
        formatConfig ?? null,
        matchConfig ?? null,
        playerCount,
        firstPlayer,
      );
    }
    return this.fallback!.initializeGame(
      deckData ?? null,
      seed,
      formatConfig ?? null,
      matchConfig ?? null,
      playerCount,
      firstPlayer,
    );
  }

  /** Expose the worker client for AI pool state export (Phase 4). */
  getEngineClient(): EngineWorkerClient | null {
    return this.engine;
  }

  private assertInitialized(): void {
    if (!this.initialized) {
      throw new AdapterError(
        AdapterErrorCode.NOT_INITIALIZED,
        "Adapter not initialized. Call initialize() first.",
        true,
      );
    }
  }
}

// ── Main-Thread Fallback ─────────────────────────────────────────────────
// Only used when Web Worker creation fails.

interface MainThreadFallback {
  ensureCardDatabase(): Promise<number>;
  submitAction(action: GameAction, actor: PlayerId): Promise<SubmitResult>;
  submitInteraction(submission: InteractionSubmission, actor: PlayerId): Promise<SubmitResult>;
  previewManaPayment(action: GameAction, actor: PlayerId): Promise<ObjectId[]>;
  getState(): Promise<GameState>;
  getFilteredState(viewerId: number): Promise<GameState>;
  getLegalActions(): Promise<LegalActionsResult>;
  getSnapshot(): Promise<{ state: GameState; legalResult: LegalActionsResult }>;
  getLegalActionsForViewer(viewerId: number): Promise<LegalActionsResult>;
  getViewerSnapshot(viewerId: number): Promise<ViewerSnapshot>;
  getAiActionProposal(difficulty: string, playerId: number): Promise<AiActionProposal | null>;
  submitAiActionProposal(proposal: AiActionProposal): Promise<AiProposalSubmission>;
  exportState(): Promise<string>;
  restoreState(stateJson: string): Promise<void>;
  resumeMultiplayerHostState(stateJson: string): Promise<void>;
  setMultiplayerMode(enabled: boolean): void;
  applySeatMutation(stateJson: string, mutationJson: string): Promise<unknown>;
  projectSeatView(stateJson: string): Promise<unknown>;
  ping(): string;
  initializeGame(
    deckData: unknown | null,
    seed: number,
    formatConfig: FormatConfig | null,
    matchConfig: MatchConfig | null,
    playerCount?: number,
    firstPlayer?: number,
  ): Promise<SubmitResult>;
  estimateBracketForDeck(deck: BracketDeckRequest): Promise<BracketEstimate | null>;
  evaluateDeckCompatibility(request: unknown): Promise<unknown>;
  getCardFaceData(cardName: string): Promise<unknown>;
  getCardParseDetails(cardName: string): Promise<unknown>;
  getCardRulings(cardName: string): Promise<unknown>;
}

async function createMainThreadFallback(): Promise<MainThreadFallback> {
  const wasm = await import("@wasm/engine");
  const cardData = await import("../services/cardData");
  await cardData.ensureWasmInit();

  let queue: Promise<void> = Promise.resolve();

  function enqueue<T>(operation: () => T): Promise<T> {
    const p = queue.then(() => operation());
    queue = p.then(
      () => undefined,
      () => undefined,
    );
    return p;
  }

  return {
    ensureCardDatabase: () => cardData.ensureCardDatabase(),

    submitAction: (action: GameAction, actor: PlayerId) =>
      enqueue(() => {
        const r = wasm.submit_action(actor, action);
        if (typeof r === "string") throw new Error(r);
        return { events: r.events ?? [], log_entries: r.log_entries ?? [] };
      }),

    submitInteraction: (submission: InteractionSubmission, actor: PlayerId) =>
      enqueue(() => {
        const r = wasm.submit_interaction_js(actor, submission);
        if (typeof r === "string") throw new Error(r);
        return { events: r.events ?? [], log_entries: r.log_entries ?? [] };
      }),

    previewManaPayment: (action: GameAction, actor: PlayerId) =>
      enqueue(() => {
        const sources = wasm.preview_mana_payment_js(actor, action);
        if (typeof sources === "string") throw new Error(sources);
        return sources as ObjectId[];
      }),

    // null from any of these three getters means WASM `GAME_STATE` is None
    // (worker restart, PWA update desync, panic recovery). Throw with the
    // Rust sentinel so the adapter's classifyEngineError escalates to
    // STATE_LOST. Previously we substituted defaults here, which silently
    // poisoned IndexedDB via dispatch.ts's saveGame call.
    getState: () =>
      enqueue(() => {
        const s = wasm.get_game_state();
        if (s === null) throw new Error("NOT_INITIALIZED: get_game_state returned null");
        return s as GameState;
      }),

    getFilteredState: (viewerId: number) =>
      enqueue(() => {
        const s = wasm.get_filtered_game_state(viewerId);
        if (s === null) throw new Error("NOT_INITIALIZED: get_filtered_game_state returned null");
        return s as GameState;
      }),

    getLegalActions: () =>
      enqueue(() => {
        const r = wasm.get_legal_actions_js();
        if (r === null) throw new Error("NOT_INITIALIZED: get_legal_actions_js returned null");
        return r as LegalActionsResult;
      }),

    // Same atomicity guarantee as the worker's `getSnapshot` case: both WASM
    // exports are synchronous and run back-to-back inside ONE `enqueue`
    // callback, so no other queued operation (notably `submit_action`) can
    // interleave between them.
    getSnapshot: () =>
      enqueue(() => {
        const s = wasm.get_game_state();
        const r = wasm.get_legal_actions_js();
        if (s === null || r === null) {
          throw new Error("NOT_INITIALIZED: get_game_state/get_legal_actions_js returned null");
        }
        return { state: s as GameState, legalResult: r as LegalActionsResult };
      }),

    getLegalActionsForViewer: (viewerId: number) =>
      enqueue(() => {
        const r = wasm.get_legal_actions_for_viewer_js(viewerId);
        if (r === null) throw new Error("NOT_INITIALIZED: get_legal_actions_for_viewer_js returned null");
        return r as LegalActionsResult;
      }),

    getViewerSnapshot: (viewerId: number) =>
      enqueue(() => {
        const r = wasm.get_viewer_snapshot_js(viewerId);
        if (r === null) throw new Error("NOT_INITIALIZED: get_viewer_snapshot_js returned null");
        return r as ViewerSnapshot;
      }),

    getAiActionProposal: (difficulty: string, playerId: number) =>
      enqueue(() => (wasm.get_ai_action_proposal(difficulty, playerId) ?? null) as AiActionProposal | null),

    submitAiActionProposal: (proposal: AiActionProposal) =>
      enqueue(() => wasm.submit_ai_action_proposal(
        proposal.token,
        proposal.actor,
        proposal.action,
      ) as AiProposalSubmission),

    exportState: () => enqueue(() => wasm.export_game_state_json()),

    restoreState: (stateJson: string) =>
      enqueue(() => wasm.restore_game_state(stateJson)),

    resumeMultiplayerHostState: (stateJson: string) =>
      enqueue(() => wasm.resume_multiplayer_host_state(stateJson)),

    setMultiplayerMode: (enabled: boolean) => {
      enqueue(() => wasm.set_multiplayer_mode(enabled));
    },

    applySeatMutation: (stateJson: string, mutationJson: string) =>
      enqueue(() => wasm.apply_seat_mutation(stateJson, mutationJson)),

    projectSeatView: (stateJson: string) =>
      enqueue(() => wasm.project_seat_view(stateJson)),

    ping: () => wasm.ping(),

    initializeGame: (
      deckData: unknown | null,
      seed: number,
      formatConfig: FormatConfig | null,
      matchConfig: MatchConfig | null,
      playerCount?: number,
      firstPlayer?: number,
    ) =>
      enqueue(() => {
        const r = wasm.initialize_game(
          deckData,
          seed,
          formatConfig,
          matchConfig,
          playerCount ?? undefined,
          firstPlayer ?? undefined,
        );
        if (r && typeof r === "object" && "error" in r && r.error) {
          const envelope = r as { reasons?: string[]; cedh_bracket_violation?: boolean };
          const reasons = envelope.reasons ?? [];
          const message = `Deck validation failed: ${reasons.join("; ")}`;
          if (envelope.cedh_bracket_violation) {
            throw new AdapterError(
              AdapterErrorCode.BRACKET_VIOLATION,
              envelope.reasons?.join("; ") ?? "cEDH bracket violation",
              false,
            );
          }
          throw new Error(message);
        }
        return { events: r.events ?? [], log_entries: r.log_entries ?? [] };
      }),

    estimateBracketForDeck: (deck: BracketDeckRequest) =>
      enqueue(() => {
        const r = wasm.estimate_bracket_for_deck(deck);
        if (r === null || r === undefined) return null;
        if (isBracketEstimate(r)) return r;
        throw new Error("estimate_bracket_for_deck returned an invalid bracket estimate");
      }),

    // Card DB is loaded into this same `@wasm/engine` module singleton by
    // `ensureCardDatabase` (engineRuntime), so the query reads it directly.
    evaluateDeckCompatibility: (request: unknown) =>
      enqueue(() => wasm.evaluate_deck_compatibility_js(request)),

    getCardFaceData: (cardName: string) =>
      enqueue(() => wasm.get_card_face_data(cardName)),

    getCardParseDetails: (cardName: string) =>
      enqueue(() => wasm.get_card_parse_details(cardName)),

    getCardRulings: (cardName: string) =>
      enqueue(() => wasm.get_card_rulings(cardName)),
  };
}
