import type {
  BatchResolveResult,
  EngineAdapter,
  FormatConfig,
  GameAction,
  GameState,
  LegalActionsResult,
  MatchConfig,
  PlayerId,
  SubmitResult,
  ViewerSnapshot,
} from "./types";
import { AdapterError, AdapterErrorCode, isStateLostMessage } from "./types";
import { EngineWorkerClient } from "./engine-worker-client";
import { AiWorkerPool } from "./ai-worker-pool";

/**
 * Flatten the `ClientGameState { state, derived }` wire envelope produced
 * by the engine's WASM getters into the store-side `GameState` shape with
 * `derived` attached as an optional field. When the runtime returns a
 * plain `GameState` (older WASM build, post-state-loss sentinel), the
 * wrapped shape is absent and we pass through untouched.
 *
 * See `crates/engine/src/game/derived_views.rs`.
 */
function unwrapClientGameState(raw: unknown): GameState {
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

  // Multi-worker AI pool for VeryHard root parallelism (lazy-initialized)
  private aiPool: AiWorkerPool | null = null;
  private aiPoolFailed = false;

  // Fallback: direct WASM on main thread (only used if Worker fails)
  private fallback: MainThreadFallback | null = null;

  async initialize(): Promise<void> {
    if (this.initialized) return;
    try {
      this.engine = new EngineWorkerClient();
      await this.engine.initialize();
    } catch {
      // Worker creation failed — fall back to main-thread WASM
      console.warn(
        "Web Worker creation failed, falling back to main-thread WASM",
      );
      this.engine = null;
      this.fallback = await createMainThreadFallback();
    }
    this.initialized = true;
  }

  private async ensureCardDb(): Promise<void> {
    if (this.cardDbLoaded) return;
    try {
      if (this.engine) {
        const count = await this.engine.loadCardDbFromUrl();
        console.log(`Card database loaded in worker: ${count} cards`);
      } else if (this.fallback) {
        const count = await this.fallback.ensureCardDatabase();
        console.log(`Card database loaded: ${count} cards`);
      }
      this.cardDbLoaded = true;
      // Also load into AI pool if it's already initialized
      if (this.aiPool && !this.aiPool.isCardDbLoaded) {
        await this.aiPool.loadCardDb();
      }
    } catch (err) {
      console.warn("Failed to load card database:", err);
    }
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

  async getAiAction(
    difficulty: string,
    playerId: number,
  ): Promise<GameAction | null> {
    this.assertInitialized();

    // Root parallelism for VeryHard: multiple workers score independently, merge results
    if (difficulty === "VeryHard" && this.engine) {
      const pool = await this.ensureAiPool();
      if (pool) {
        try {
          const stateJson = await this.engine.exportState();
          const merged = await pool.getAiScoredCandidates(
            stateJson,
            difficulty,
            playerId,
          );
          if (merged && merged.length > 0) {
            if (merged.length === 1) return merged[0][0];
            // Delegate softmax selection to Rust (keeps all AI logic in the engine)
            const scoresJson = JSON.stringify(merged);
            return this.engine.selectActionFromScores(
              scoresJson,
              difficulty,
              Date.now(),
            );
          }
        } catch (err) {
          // STATE_LOST / ENGINE_PANIC must escalate immediately — falling
          // through to the single-worker path would just hit the same sentinel
          // (or panic) and waste a round-trip. The async classifier drains
          // the panic from the engine worker so callers see ENGINE_PANIC
          // when applicable. All other pool failures are recoverable via
          // the single-worker fallback.
          if (err instanceof Error && isStateLostMessage(err.message)) {
            throw await classifyEngineErrorAsync(err, this.takePanic);
          }
        }
      }
    }

    // Single-worker path for non-VeryHard or when pool unavailable
    try {
      if (this.engine) return await this.engine.getAiAction(difficulty, playerId);
      return await this.fallback!.getAiAction(difficulty, playerId);
    } catch (err) {
      throw await classifyEngineErrorAsync(err, this.takePanic);
    }
  }

/** Lazy AI pool init — only created on first VeryHard request. */
  private async ensureAiPool(): Promise<AiWorkerPool | null> {
    if (this.aiPool) return this.aiPool;
    if (this.aiPoolFailed) return null;
    try {
      const cores = navigator.hardwareConcurrency ?? 0;
      const count = Math.max(2, Math.min(cores - 1, 4));
      this.aiPool = new AiWorkerPool(count);
      await this.aiPool.initialize();
      if (this.cardDbLoaded) {
        await this.aiPool.loadCardDb();
      }
      return this.aiPool;
    } catch {
      this.aiPoolFailed = true;
      return null;
    }
  }

  /**
   * Get AI actions for multiple AI seats with per-seat difficulty.
   * Returns the action for the AI player whose turn it currently is, or null.
   */
  getAiActionForSeats(
    aiSeats: { playerId: number; difficulty: string }[],
    activePlayer: number,
  ): Promise<GameAction | null> {
    const seat = aiSeats.find((s) => s.playerId === activePlayer);
    if (!seat) return Promise.resolve(null);
    return this.getAiAction(seat.difficulty, seat.playerId);
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

  async restoreState(state: GameState): Promise<void> {
    this.assertInitialized();
    await this.ensureCardDb();
    const json = JSON.stringify(state);
    if (this.engine) await this.engine.restoreState(json);
    else await this.fallback!.restoreState(json);
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
  async resumeMultiplayerHostState(state: GameState): Promise<void> {
    this.assertInitialized();
    const json = JSON.stringify(state);
    if (this.engine) {
      // Ensure the card database is loaded before the engine rehydrates
      // ability definitions on restore. Same sequential-queue guarantee
      // as `restoreState`.
      if (!this.cardDbLoaded) {
        await this.engine.loadCardDbFromUrl().then(
          () => { this.cardDbLoaded = true; },
          () => { /* card DB is best-effort */ },
        );
      }
      await this.engine.resumeMultiplayerHostState(json);
    } else {
      this.fallback!.resumeMultiplayerHostState(json);
    }
  }

  /**
   * Clear the WASM game state without terminating the worker.
   *
   * Preserves the WASM instance (with V8 TurboFan optimizations), card database,
   * and AI worker pool. Any in-flight AI computation on the old state will
   * short-circuit with an error rather than running a full search.
   */
  async resetGameState(): Promise<void> {
    if (this.engine) {
      await this.engine.resetGame();
    }
  }

  dispose(): void {
    // Clear the singleton reference so getSharedAdapter() creates a fresh
    // instance if called after dispose (e.g., error recovery code paths).
    if (sharedAdapter === this) sharedAdapter = null;
    this.engine?.dispose();
    this.engine = null;
    this.aiPool?.dispose();
    this.aiPool = null;
    this.aiPoolFailed = false;
    this.fallback = null;
    this.initialized = false;
    this.cardDbLoaded = false;
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
  getState(): Promise<GameState>;
  getFilteredState(viewerId: number): Promise<GameState>;
  getLegalActions(): Promise<LegalActionsResult>;
  getLegalActionsForViewer(viewerId: number): Promise<LegalActionsResult>;
  getViewerSnapshot(viewerId: number): Promise<ViewerSnapshot>;
  getAiAction(difficulty: string, playerId: number): Promise<GameAction | null>;
  restoreState(stateJson: string): Promise<void>;
  resumeMultiplayerHostState(stateJson: string): void;
  setMultiplayerMode(enabled: boolean): void;
  applySeatMutation(stateJson: string, mutationJson: string): Promise<unknown>;
  ping(): string;
  initializeGame(
    deckData: unknown | null,
    seed: number,
    formatConfig: FormatConfig | null,
    matchConfig: MatchConfig | null,
    playerCount?: number,
    firstPlayer?: number,
  ): Promise<SubmitResult>;
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

    getAiAction: (difficulty: string, playerId: number) =>
      enqueue(() => {
        const r = wasm.get_ai_action(difficulty, playerId);
        return (r ?? null) as GameAction | null;
      }),

    restoreState: (stateJson: string) =>
      enqueue(() => wasm.restore_game_state(stateJson)),

    resumeMultiplayerHostState: (stateJson: string) => {
      enqueue(() => wasm.resume_multiplayer_host_state(stateJson));
    },

    setMultiplayerMode: (enabled: boolean) => {
      enqueue(() => wasm.set_multiplayer_mode(enabled));
    },

    applySeatMutation: (stateJson: string, mutationJson: string) =>
      enqueue(() => wasm.apply_seat_mutation(stateJson, mutationJson)),

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
          const reasons = (r as { reasons?: string[] }).reasons ?? [];
          throw new Error(`Deck validation failed: ${reasons.join("; ")}`);
        }
        return { events: r.events ?? [], log_entries: r.log_entries ?? [] };
      }),
  };
}
