import type {
  EngineAdapter,
  FormatConfig,
  GameAction,
  GameState,
  LegalActionsResult,
  MatchConfig,
  PlayerId,
  SubmitResult,
} from "./types";
import { AdapterError, AdapterErrorCode } from "./types";
import { EngineWorkerClient } from "./engine-worker-client";
import { AiWorkerPool } from "./ai-worker-pool";

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

  async submitAction(action: GameAction, actor: PlayerId): Promise<SubmitResult> {
    this.assertInitialized();
    if (this.engine) {
      return this.engine.submitAction(actor, action);
    }
    return this.fallback!.submitAction(action, actor);
  }

  async getState(): Promise<GameState> {
    this.assertInitialized();
    if (this.engine) {
      return this.engine.getState();
    }
    return this.fallback!.getState();
  }

  async getFilteredState(viewerId: number): Promise<GameState> {
    this.assertInitialized();
    if (this.engine) {
      return this.engine.getFilteredState(viewerId);
    }
    return this.fallback!.getFilteredState(viewerId);
  }

  async getLegalActions(): Promise<LegalActionsResult> {
    this.assertInitialized();
    if (this.engine) {
      return this.engine.getLegalActions();
    }
    return this.fallback!.getLegalActions();
  }

  async getAiAction(
    difficulty: string,
    playerId = 1,
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
        } catch {
          // Pool failed — fall through to single-worker path
        }
      }
    }

    // Single-worker path for non-VeryHard or when pool unavailable
    if (this.engine) {
      return this.engine.getAiAction(difficulty, playerId);
    }
    return this.fallback!.getAiAction(difficulty, playerId);
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

  restoreState(state: GameState): void {
    this.assertInitialized();
    const json = JSON.stringify(state);
    if (this.engine) {
      // Ensure the card database is loaded in the worker before restoring,
      // so rehydrate_game_from_card_db can refresh ability definitions.
      // Both messages are sequential in the worker queue — loadCardDbFromUrl
      // completes before restoreState runs, no await needed.
      if (!this.cardDbLoaded) {
        this.engine.loadCardDbFromUrl().then(
          () => { this.cardDbLoaded = true; },
          () => { /* card DB is best-effort for resume */ },
        );
      }
      this.engine.restoreState(json);
    } else {
      this.fallback!.restoreState(json);
    }
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
  getAiAction(difficulty: string, playerId: number): Promise<GameAction | null>;
  restoreState(stateJson: string): void;
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

    getState: () =>
      enqueue(() => {
        const s = wasm.get_game_state();
        return (s === null ? wasm.create_initial_state() : s) as GameState;
      }),

    getFilteredState: (viewerId: number) =>
      enqueue(() => {
        const s = wasm.get_filtered_game_state(viewerId);
        return (s === null ? wasm.create_initial_state() : s) as GameState;
      }),

    getLegalActions: () =>
      enqueue(() => {
        const r = wasm.get_legal_actions_js();
        return (r === null ? { actions: [], autoPassRecommended: false } : r) as LegalActionsResult;
      }),

    getAiAction: (difficulty: string, playerId: number) =>
      enqueue(() => {
        const r = wasm.get_ai_action(difficulty, playerId);
        return (r ?? null) as GameAction | null;
      }),

    restoreState: (stateJson: string) => {
      enqueue(() => wasm.restore_game_state(stateJson));
    },

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
