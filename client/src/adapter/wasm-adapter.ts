import type {
  AiActionProposal,
  AiDecisionDiagnosticReceipt,
  AiDecisionDiagnosticsCapability,
  AiProposalSubmission,
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
  RestoredGameStateResult,
  RestoredStackAutomationPresentation,
  SubmitResult,
  ViewerSnapshot,
} from "./types";
import type {
  InteractionPreview,
  InteractionPreviewRequest,
  InteractionSubmission,
} from "./generated/interaction";
import {
  actionRejectionError,
  AdapterError,
  AdapterErrorCode,
  isActionOutcome,
  isStateLostMessage,
  nextSnapshotSeq,
} from "./types";
import type { BracketDeckRequest, BracketEstimate } from "../types/bracketEstimate";
import { isBracketEstimate } from "../types/bracketEstimate";
import { EngineWorkerClient } from "./engine-worker-client";
import { classifyInitFailure } from "./init-envelope";
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

const INITIALIZATION_CANCELED_MESSAGE = "Adapter initialization was canceled. Please try again.";

// Parallel scoring is optional. Bound its queued restore-and-score work so a
// stalled score worker cannot make a healthy local game appear hung.
const AI_POOL_SCORE_TIMEOUT_MS = 5_000;
const DEBUG_CREATE_CARD_DB_MISSING = "Engine error: card database not loaded";

function isDebugCreateCard(action: GameAction): boolean {
  return action.type === "Debug" && action.data.type === "CreateCard";
}

function isDebugCreateCardDbMissing(error: unknown): boolean {
  return error instanceof Error && error.message === DEBUG_CREATE_CARD_DB_MISSING;
}

function unwrapActionOutcome<T>(value: unknown): T {
  if (typeof value === "string") throw new Error(value);
  if (!isActionOutcome(value)) {
    throw new AdapterError(
      AdapterErrorCode.ACTION_REJECTED,
      "The engine rejected that action.",
      true,
    );
  }
  if (value.status === "rejected") {
    throw actionRejectionError(value.rejection);
  }
  return value.result as T;
}

class AiPoolScoreTimeoutError extends Error {
  constructor() {
    super(`AI worker pool timed out after ${AI_POOL_SCORE_TIMEOUT_MS}ms`);
    this.name = "AiPoolScoreTimeoutError";
  }
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
  if (err instanceof AdapterError) return err;
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
 * Engine for a P2P host session. Memory-constrained devices reuse the shared
 * worker (one WASM module + one card DB for the whole tab) and pay for it in
 * serialized engine calls; everywhere else the host keeps a private worker so
 * its authoritative game state can never interleave with local play. Mirrors
 * the AI-pool trade at `ensureAiPool`, which likewise returns null on these
 * devices rather than spending a second resident allocation.
 */
export function getHostAdapter(): WasmAdapter {
  return isMemoryConstrainedDevice() ? getSharedAdapter() : new WasmAdapter();
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
/** How much of a card-database load failure to put in a user-facing message.
 *  serde's "unknown variant" error enumerates every variant of the enum it
 *  rejected, which runs to thousands of characters; everything diagnostic is in
 *  the opening clause, so the tail is noise in a toast. */
const CARD_DB_ERROR_MAX_CHARS = 180;

function describeCardDbError(err: unknown): string {
  const message = err instanceof Error ? err.message : String(err ?? "");
  if (!message) return "";
  const trimmed =
    message.length > CARD_DB_ERROR_MAX_CHARS
      ? `${message.slice(0, CARD_DB_ERROR_MAX_CHARS)}…`
      : message;
  return `: ${trimmed}`;
}

export class WasmAdapter implements EngineAdapter, AiDecisionDiagnosticsCapability {
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
  private aiDecisionDiagnosticsEnabled = false;
  private aiDecisionDiagnosticsEpoch = 0;
  private readonly receiptByToken = new Map<string, AiDecisionDiagnosticReceipt>();
  private readonly tokenBySemanticOwner = new Map<PlayerId, string>();
  private readonly aiDecisionDiagnosticListeners = new Set<(receipt: AiDecisionDiagnosticReceipt) => void>();

  // ── #7920: pod-issued whole-match concede ──────────────────────────────
  // Mirrors the P2P adapters' conditional MatchConcedeCapability: the duck-
  // typed `supportsMatchConcede(adapter)` guard passes only after a pod
  // match binding installs the members. Plain AI games never bind it, so
  // their menu keeps the engine-dispatch concede path.
  supportsMatchConcede?: true;
  sendMatchConcede?: () => void;

  /** Installed only by a pod-issued bot-match binding (multiplayerDraftStore). */
  bindMatchConcede(run: () => void): void {
    this.supportsMatchConcede = true;
    this.sendMatchConcede = run;
  }

  /** Invalidate local observations whenever the WASM authority invalidates proposals. */
  private invalidateAiDecisionDiagnostics(): void {
    this.aiDecisionDiagnosticsEpoch += 1;
    this.receiptByToken.clear();
    this.tokenBySemanticOwner.clear();
  }

  setAiDecisionDiagnosticsEnabled(enabled: boolean): void {
    if (this.aiDecisionDiagnosticsEnabled === enabled) return;
    this.aiDecisionDiagnosticsEnabled = enabled;
    this.invalidateAiDecisionDiagnostics();
  }

  subscribeAiDecisionDiagnostics(listener: (receipt: AiDecisionDiagnosticReceipt) => void): () => void {
    this.aiDecisionDiagnosticListeners.add(listener);
    return () => this.aiDecisionDiagnosticListeners.delete(listener);
  }

  private retainAiDecisionDiagnostic(
    startEpoch: number,
    proposal: AiActionProposal,
    receipt: AiDecisionDiagnosticReceipt,
  ): void {
    if (!this.aiDecisionDiagnosticsEnabled || startEpoch !== this.aiDecisionDiagnosticsEpoch) return;
    const previous = this.tokenBySemanticOwner.get(proposal.semanticOwner);
    if (previous) this.receiptByToken.delete(previous);
    this.tokenBySemanticOwner.set(proposal.semanticOwner, proposal.token);
    this.receiptByToken.set(proposal.token, receipt);
  }

  private takeAiDecisionDiagnostic(token: string): AiDecisionDiagnosticReceipt | undefined {
    const receipt = this.receiptByToken.get(token);
    if (!receipt) return undefined;
    this.receiptByToken.delete(token);
    if (this.tokenBySemanticOwner.get(receipt.semanticOwner) === token) {
      this.tokenBySemanticOwner.delete(receipt.semanticOwner);
    }
    return receipt;
  }

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
          throw new AdapterError(
            AdapterErrorCode.NOT_INITIALIZED,
            INITIALIZATION_CANCELED_MESSAGE,
            true,
          );
        }
        this.engine = candidateEngine;
      } catch (error) {
        candidateEngine?.dispose();
        if (this.lifecycleGeneration !== generation) {
          throw new AdapterError(
            AdapterErrorCode.NOT_INITIALIZED,
            INITIALIZATION_CANCELED_MESSAGE,
            true,
          );
        }
        // Worker creation or initialization failed — fall back to main-thread WASM
        console.warn(
          "Web Worker initialization failed, falling back to main-thread WASM",
          error,
        );
        const candidateFallback = await createMainThreadFallback();
        if (this.lifecycleGeneration !== generation) {
          throw new AdapterError(
            AdapterErrorCode.NOT_INITIALIZED,
            INITIALIZATION_CANCELED_MESSAGE,
            true,
          );
        }
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

  // Why the failure is kept rather than only logged: `ensureCardDb` is
  // best-effort by design and resolves even when the load failed, so callers
  // that require the database cannot tell *why* it is absent. Without this the
  // engine worker answers them with "Card database not loaded", which names a
  // missing call rather than the real cause -- a schema-rejected pool reads as
  // a forgotten `loadCardDb`.
  private cardDbError: unknown = null;

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
        this.cardDbError = null;
      } catch (err) {
        this.cardDbError = err;
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
    try {
      const submit = () => this.engine
        ? this.engine.submitAction(actor, action)
        : this.fallback!.submitAction(action, actor);
      let result: SubmitResult;
      try {
        result = await submit();
      } catch (error) {
        if (!isDebugCreateCard(action) || !isDebugCreateCardDbMissing(error)) throw error;
        await this.ensureCardDb();
        result = await submit();
      }
      this.invalidateAiDecisionDiagnostics();
      return result;
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
      const result = this.engine ? await this.engine.submitInteraction(actor, submission) : await this.fallback!.submitInteraction(submission, actor);
      this.invalidateAiDecisionDiagnostics();
      return result;
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

  async previewInteraction(
    request: InteractionPreviewRequest,
    actor: PlayerId,
  ): Promise<InteractionPreview> {
    this.assertInitialized();
    try {
      if (this.engine) return await this.engine.previewInteraction(actor, request);
      return await this.fallback!.previewInteraction(request, actor);
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
      const captureEpoch = this.aiDecisionDiagnosticsEpoch;
      const capture = this.aiDecisionDiagnosticsEnabled;
      if (capture) {
        // Preserve the existing VeryHard score-worker route. Capturing may
        // observe its rebinding receipt, but never chooses a different path.
        if (difficulty === "VeryHard" && this.engine) {
          try {
            const state = unwrapClientGameState(await this.engine!.getState());
            if (state.waiting_for.type === "Priority") {
              const scores = await this.getAiPoolScores(this.engine, difficulty, playerId);
              if (scores?.length) {
                const captured = await this.engine!.getAiActionProposalFromScoresWithDiagnostics(
                  JSON.stringify(scores),
                  difficulty,
                  playerId,
                  Date.now(),
                );
                if (captured) {
                  this.retainAiDecisionDiagnostic(captureEpoch, captured.proposal, captured.receipt);
                  return captured.proposal;
                }
              }
            }
          } catch (error) {
            if (error instanceof Error && isStateLostMessage(error.message)) throw error;
            if (error instanceof AiPoolScoreTimeoutError) {
              const captured = await this.engine!.getAiTacticalActionProposalWithDiagnostics(
                difficulty,
                playerId,
              );
              if (captured) {
                this.retainAiDecisionDiagnostic(captureEpoch, captured.proposal, captured.receipt);
                return captured.proposal;
              }
            }
            console.warn("AI worker pool failed; using authoritative single worker", error);
          }
        }
        const captured = this.engine
          ? await this.engine.getAiActionProposalWithDiagnostics(difficulty, playerId)
          : await this.fallback!.getAiActionProposalWithDiagnostics(difficulty, playerId);
        if (captured) this.retainAiDecisionDiagnostic(captureEpoch, captured.proposal, captured.receipt);
        return captured?.proposal ?? null;
      }
      if (difficulty === "VeryHard" && this.engine) {
        try {
          // A snapshot can become stale while scoring. That is safe: the main
          // worker rebinds every score against a newly-issued contract below.
          const state = unwrapClientGameState(await this.engine.getState());
          if (state.waiting_for.type === "Priority") {
            const scores = await this.getAiPoolScores(this.engine, difficulty, playerId);
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
        } catch (error) {
          if (error instanceof Error && isStateLostMessage(error.message)) throw error;
          if (error instanceof AiPoolScoreTimeoutError) {
            const proposal = await this.engine.getAiTacticalActionProposal(difficulty, playerId);
            if (proposal) return proposal;
          }
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
      const outcome = this.engine
        ? await this.engine.submitAiActionProposal(proposal)
        : await this.fallback!.submitAiActionProposal(proposal);
      if (outcome.status === "applied") {
        const receipt = this.takeAiDecisionDiagnostic(proposal.token);
        if (receipt && this.aiDecisionDiagnosticsEnabled) {
          for (const listener of this.aiDecisionDiagnosticListeners) listener(receipt);
        }
        this.invalidateAiDecisionDiagnostics();
      } else if (outcome.status === "stale") {
        this.takeAiDecisionDiagnostic(proposal.token);
      }
      return outcome;
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

  /** Discard an optional scorer without affecting the authoritative worker. */
  private disableAiPool(generation: number): void {
    if (generation !== this.aiPoolGeneration) return;
    this.aiPoolGeneration += 1;
    this.aiPoolPromise = null;
    this.aiPool?.dispose();
    this.aiPool = null;
    this.aiPoolFailed = true;
  }

  private async getAiPoolScores(
    engine: EngineWorkerClient,
    difficulty: string,
    playerId: number,
  ): Promise<[GameAction, number][] | null> {
    const pool = await this.ensureAiPool();
    if (!pool) return null;
    const generation = this.aiPoolGeneration;
    const stateJson = await engine.exportState();
    if (generation !== this.aiPoolGeneration) return null;
    let timeoutId: ReturnType<typeof setTimeout> | undefined;

    try {
      return await Promise.race([
        pool.getAiScoredCandidates(stateJson, difficulty, playerId),
        new Promise<never>((_, reject) => {
          timeoutId = setTimeout(() => {
            reject(new AiPoolScoreTimeoutError());
          }, AI_POOL_SCORE_TIMEOUT_MS);
        }),
      ]);
    } catch (error) {
      this.disableAiPool(generation);
      throw error;
    } finally {
      if (timeoutId !== undefined) clearTimeout(timeoutId);
    }
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

  private async requireCardDb(): Promise<void> {
    await this.ensureCardDb();
    // Soft-failed ensureCardDb leaves cardDbLoaded false and skips
    // rehydrate_game_from_card_db — restored CardName NamedChoices then have
    // empty legal actions and softlock the AI (#6393). Refuse DB-less restore
    // / P2P host resume the same way warmCardDatabase surfaces load failure.
    // Every caller that reads CARD_DB goes through here, so the cause travels
    // with the refusal instead of staying behind in a console warning.
    if (!this.cardDbLoaded) {
      // `new Error(msg, { cause })` is ES2022 and this bundle targets ES2020,
      // so the cause is attached the way `network/connection.ts` does it.
      const error = new Error(
        `Card database failed to load${describeCardDbError(this.cardDbError)}`,
      );
      Object.assign(error, { cause: this.cardDbError });
      throw error;
    }
  }

  async restoreState(state: PersistedGameState): Promise<void> {
    this.assertInitialized();
    await this.requireCardDb();
    const json = JSON.stringify(state);
    if (this.engine) await this.engine.restoreState(json);
    else await this.fallback!.restoreState(json);
    this.invalidateAiDecisionDiagnostics();
  }

  async resumeRestoredGameState(): Promise<RestoredGameStateResult> {
    this.assertInitialized();
    try {
      const resumed = this.engine
        ? await this.engine.resumeRestoredGameState()
        : await this.fallback!.resumeRestoredGameState();
      this.invalidateAiDecisionDiagnostics();
      return {
        presentation: resumed.presentation,
        snapshot: {
          state: unwrapClientGameState(resumed.snapshot.state),
          legalResult: resumed.snapshot.legalResult,
          seq: nextSnapshotSeq(),
        },
      };
    } catch (err) {
      throw await classifyEngineErrorAsync(err, this.takePanic);
    }
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
   * Set the engine's multiplayer enforcement flag. While it is set, the Rust
   * side refuses `restore_game_state` (undo) and refuses a local
   * `initializeGame` — defense against any caller rewriting a multiplayer
   * game.
   *
   * Nothing in the client turns it *on*: the engine claims it itself, in the
   * same call that installs the host's game
   * (`initializeMultiplayerHostGame`, `resumeMultiplayerHostState`). This
   * method exists for the release side — `releaseHostSession` clears the flag
   * when a host session ends.
   */
  async setMultiplayerMode(enabled: boolean): Promise<void> {
    this.assertInitialized();
    if (this.engine) {
      await this.engine.setMultiplayerMode(enabled);
    } else {
      this.fallback!.setMultiplayerMode(enabled);
    }
    this.invalidateAiDecisionDiagnostics();
  }

  async applySeatMutation(stateJson: string, mutationJson: string): Promise<unknown> {
    this.assertInitialized();
    // No `ensureCardDb()` here: `apply_seat_mutation` never reads CARD_DB. Its
    // `WasmDeckResolver` resolves only against the static `STARTER_DECKS` table
    // (crates/engine/src/starter_decks.rs) and otherwise clones the passed-in
    // name list, staying at the name-only layer — `initialize_game` re-resolves
    // against CARD_DB when the game actually starts. (The Rust doc comment on
    // `apply_seat_mutation` claiming it uses "the TLS card database" is stale;
    // read the resolver, not the doc comment.) Warming a ~100 MB DB for every
    // lobby seat change is pure cost, and on a host lobby it is a second
    // resident copy alongside the shared worker's.
    if (this.engine) {
      const result = await this.engine.applySeatMutation(stateJson, mutationJson);
      this.invalidateAiDecisionDiagnostics();
      return result;
    }
    const result = await this.fallback!.applySeatMutation(stateJson, mutationJson);
    this.invalidateAiDecisionDiagnostics();
    return result;
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
  async resumeMultiplayerHostState(state: PersistedGameState): Promise<RestoredGameStateResult> {
    this.assertInitialized();
    // Same CARD_DB requirement as restoreState — resume rehydrates abilities
    // only when the DB is loaded (engine-wasm resume_multiplayer_host_state).
    await this.requireCardDb();
    const json = JSON.stringify(state);
    const resumed = this.engine
      ? await this.engine.resumeMultiplayerHostState(json)
      : await this.fallback!.resumeMultiplayerHostState(json);
    this.invalidateAiDecisionDiagnostics();
    return {
      presentation: resumed.presentation,
      snapshot: {
        state: unwrapClientGameState(resumed.snapshot.state),
        legalResult: resumed.snapshot.legalResult,
        seq: nextSnapshotSeq(),
      },
    };
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
    this.invalidateAiDecisionDiagnostics();
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
    await this.requireCardDb();
  }

  /**
   * Run a stateless deck-compatibility check against the shared worker's card
   * database. Replaces the former dedicated compatibility worker — one engine
   * instance now serves both compat checks and gameplay (a CARD_DB read only,
   * no game-state mutation), mirroring the existing `estimateBracket` query.
   */
  async checkDeckCompatibility(request: unknown): Promise<unknown> {
    await this.initialize();
    await this.requireCardDb();
    if (this.engine) {
      return this.engine.evaluateDeckCompatibility(request);
    }
    return this.fallback!.evaluateDeckCompatibility(request);
  }

  /**
   * ENFORCING deck/format check. Always returns a DEFINITE verdict —
   * `{ compatible: boolean, reasons: string[] }`, never a tri-state — backed by
   * the same authoritative `validate_deck_for_format` the engine runs at game
   * creation.
   *
   * For gate callers only (today: the P2P host's per-guest deck-kick check).
   * UI-hint callers must keep using {@link checkDeckCompatibility}: that one
   * deliberately answers "no opinion" for a Custom format, because the engine
   * genuinely cannot evaluate Custom legality yet — the honest answer for a
   * legality chip, and an unacceptable one for a kick decision.
   */
  async evaluateDeckFormatGate(request: unknown): Promise<unknown> {
    await this.initialize();
    await this.requireCardDb();
    if (this.engine) {
      return this.engine.evaluateDeckFormatGate(request);
    }
    return this.fallback!.evaluateDeckFormatGate(request);
  }

  /**
   * Axis A: ask the ENGINE to capture a lobby `FormatConfig` as a saved
   * custom-format definition. Rejects (with the engine's own message) for a
   * source format whose behavior cannot be represented — Planechase, Archenemy,
   * Momir, an already-Custom source, or an empty name. Needs no card database.
   */
  async customFormatFromLobbyConfig(name: string, formatConfig: unknown): Promise<unknown> {
    await this.initialize();
    if (this.engine) {
      return this.engine.customFormatFromLobbyConfig(name, formatConfig);
    }
    return this.fallback!.customFormatFromLobbyConfig(name, formatConfig);
  }

  /**
   * The single authoritative `CustomFormatRules -> FormatConfig` resolver.
   * Total and infallible. Never assemble a Custom `FormatConfig` client-side:
   * the engine re-derives it with this same function on deserialization and
   * rejects anything that differs. Needs no card database.
   */
  async formatConfigForCustomRules(customRules: unknown): Promise<unknown> {
    await this.initialize();
    if (this.engine) {
      return this.engine.formatConfigForCustomRules(customRules);
    }
    return this.fallback!.formatConfigForCustomRules(customRules);
  }

  /**
   * Display-only card queries share the authoritative engine worker and its
   * resident card database. Keeping these off the main-thread runtime avoids a
   * second WASM module + corpus allocation when card UI mounts during gameplay.
   */
  async getCardFaceData(cardName: string): Promise<unknown> {
    await this.initialize();
    await this.requireCardDb();
    if (this.engine) return this.engine.getCardFaceData(cardName);
    return this.fallback!.getCardFaceData(cardName);
  }

  async getCardParseDetails(cardName: string): Promise<unknown> {
    await this.initialize();
    await this.requireCardDb();
    if (this.engine) return this.engine.getCardParseDetails(cardName);
    return this.fallback!.getCardParseDetails(cardName);
  }

  async getCardRulings(cardName: string): Promise<unknown> {
    await this.initialize();
    await this.requireCardDb();
    if (this.engine) return this.engine.getCardRulings(cardName);
    return this.fallback!.getCardRulings(cardName);
  }

  /**
   * End a multiplayer host session's hold on this adapter — the single
   * authority for undoing a host session.
   *
   * A private host adapter is disposed outright (today's behaviour). The
   * shared adapter keeps its worker — the card database and TurboFan-compiled
   * code stay resident for the rest of the tab — and instead clears the
   * multiplayer flag plus any game state the host installed. Branching on
   * identity (`sharedAdapter === this`) rather than a stored mode flag mirrors
   * `dispose()` below.
   *
   * `claimed` answers "did this host ever install engine state?", which only
   * the caller knows. An unclaimed host must leave the shared engine
   * completely untouched: a live local game may be running on it.
   */
  async releaseHostSession(claimed: boolean): Promise<void> {
    if (sharedAdapter !== this) {
      this.dispose();
      return;
    }
    if (!claimed) return;
    // No await between the two posts. `EngineWorkerClient.request` posts
    // inside a synchronously-executed promise executor, and neither method
    // awaits before reaching it, so the worker sees them back to back — an
    // `initializeGame` from a later mount cannot land in between.
    const flagCleared = this.setMultiplayerMode(false);
    const stateCleared = this.resetGameState();
    await Promise.all([flagCleared, stateCleared]);
  }

  dispose(): void {
    this.setAiDecisionDiagnosticsEnabled(false);
    this.aiDecisionDiagnosticListeners.clear();
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
      await this.requireCardDb();
    }
    const seed = Math.floor(Math.random() * Number.MAX_SAFE_INTEGER);
    if (this.engine) {
      const result = await this.engine.initializeGame(
        deckData ?? null,
        seed,
        formatConfig ?? null,
        matchConfig ?? null,
        playerCount,
        firstPlayer,
      );
      this.invalidateAiDecisionDiagnostics();
      return result;
    }
    const result = await this.fallback!.initializeGame(
      deckData ?? null,
      seed,
      formatConfig ?? null,
      matchConfig ?? null,
      playerCount,
      firstPlayer,
    );
    this.invalidateAiDecisionDiagnostics();
    return result;
  }

  /**
   * Start a P2P host's game. The engine refuses if it already holds a game and
   * claims the multiplayer flag in the same call that installs the state, so a
   * host sharing this worker with local play can neither destroy nor be
   * destroyed by the other session. Rejects with
   * `AdapterErrorCode.ENGINE_OCCUPIED` when the engine is occupied; nothing in
   * the engine changed on that path, so callers have nothing to compensate.
   */
  async initializeMultiplayerHostGame(
    deckData?: unknown,
    formatConfig?: FormatConfig,
    playerCount?: number,
    matchConfig?: MatchConfig,
    firstPlayer?: number,
  ): Promise<SubmitResult> {
    this.assertInitialized();
    if (deckData) {
      await this.requireCardDb();
    }
    const seed = Math.floor(Math.random() * Number.MAX_SAFE_INTEGER);
    if (this.engine) {
      const result = await this.engine.initializeMultiplayerHostGame(
        deckData ?? null,
        seed,
        formatConfig ?? null,
        matchConfig ?? null,
        playerCount,
        firstPlayer,
      );
      this.invalidateAiDecisionDiagnostics();
      return result;
    }
    const result = await this.fallback!.initializeMultiplayerHostGame(
      deckData ?? null,
      seed,
      formatConfig ?? null,
      matchConfig ?? null,
      playerCount,
      firstPlayer,
    );
    this.invalidateAiDecisionDiagnostics();
    return result;
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
  previewInteraction(
    request: InteractionPreviewRequest,
    actor: PlayerId,
  ): Promise<InteractionPreview>;
  getState(): Promise<GameState>;
  getFilteredState(viewerId: number): Promise<GameState>;
  getLegalActions(): Promise<LegalActionsResult>;
  getSnapshot(): Promise<{ state: GameState; legalResult: LegalActionsResult }>;
  getLegalActionsForViewer(viewerId: number): Promise<LegalActionsResult>;
  getViewerSnapshot(viewerId: number): Promise<ViewerSnapshot>;
  getAiActionProposal(difficulty: string, playerId: number): Promise<AiActionProposal | null>;
  getAiActionProposalWithDiagnostics(
    difficulty: string,
    playerId: number,
  ): Promise<{ proposal: AiActionProposal; receipt: AiDecisionDiagnosticReceipt } | null>;
  submitAiActionProposal(proposal: AiActionProposal): Promise<AiProposalSubmission>;
  exportState(): Promise<string>;
  restoreState(stateJson: string): Promise<void>;
  resumeRestoredGameState(): Promise<RestoredFallbackResult>;
  resumeMultiplayerHostState(stateJson: string): Promise<RestoredFallbackResult>;
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
  initializeMultiplayerHostGame(
    deckData: unknown | null,
    seed: number,
    formatConfig: FormatConfig | null,
    matchConfig: MatchConfig | null,
    playerCount?: number,
    firstPlayer?: number,
  ): Promise<SubmitResult>;
  estimateBracketForDeck(deck: BracketDeckRequest): Promise<BracketEstimate | null>;
  evaluateDeckCompatibility(request: unknown): Promise<unknown>;
  evaluateDeckFormatGate(request: unknown): Promise<unknown>;
  customFormatFromLobbyConfig(name: string, formatConfig: unknown): Promise<unknown>;
  formatConfigForCustomRules(customRules: unknown): Promise<unknown>;
  getCardFaceData(cardName: string): Promise<unknown>;
  getCardParseDetails(cardName: string): Promise<unknown>;
  getCardRulings(cardName: string): Promise<unknown>;
}

type RestoredFallbackResult = {
  presentation: RestoredStackAutomationPresentation;
  snapshot: { state: GameState; legalResult: LegalActionsResult };
};

/**
 * Raise an initialize-envelope failure as the typed error the worker path
 * raises for the same envelope (see `EngineWorkerClient`'s error handling).
 * Both fallback initialize methods go through here — the fallback is a real
 * supported path (worker creation failed), so a refusal must not surface as
 * "Deck validation failed: …" on it either.
 */
function throwInitFailure(result: unknown): void {
  const failure = classifyInitFailure(result);
  if (!failure) return;
  switch (failure.kind) {
    case "bracketViolation":
      throw new AdapterError(
        AdapterErrorCode.BRACKET_VIOLATION,
        failure.reasons.join("; ") || "cEDH bracket violation",
        false,
      );
    case "engineOccupied":
      throw new AdapterError(AdapterErrorCode.ENGINE_OCCUPIED, failure.message, false);
    case "deckValidation":
      throw new Error(failure.message);
  }
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
        const r = unwrapActionOutcome<{ events?: SubmitResult["events"]; log_entries?: SubmitResult["log_entries"] }>(
          wasm.submit_action(actor, action),
        );
        return { events: r.events ?? [], log_entries: r.log_entries ?? [] };
      }),

    submitInteraction: (submission: InteractionSubmission, actor: PlayerId) =>
      enqueue(() => {
        const r = unwrapActionOutcome<{ events?: SubmitResult["events"]; log_entries?: SubmitResult["log_entries"] }>(
          wasm.submit_interaction_js(actor, submission),
        );
        return { events: r.events ?? [], log_entries: r.log_entries ?? [] };
      }),

    previewManaPayment: (action: GameAction, actor: PlayerId) =>
      enqueue(() => {
        return unwrapActionOutcome<ObjectId[]>(wasm.preview_mana_payment_js(actor, action));
      }),

    previewInteraction: (request: InteractionPreviewRequest, actor: PlayerId) =>
      enqueue(() => {
        return unwrapActionOutcome<InteractionPreview>(
          wasm.preview_interaction_js(actor, request),
        );
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

    getAiActionProposalWithDiagnostics: (difficulty: string, playerId: number) =>
      enqueue(() => (wasm.get_ai_action_proposal_with_diagnostics(difficulty, playerId) ?? null) as {
        proposal: AiActionProposal;
        receipt: AiDecisionDiagnosticReceipt;
      } | null),

    submitAiActionProposal: (proposal: AiActionProposal) =>
      enqueue(() => wasm.submit_ai_action_proposal(
        proposal.token,
        proposal.actor,
        proposal.action,
      ) as AiProposalSubmission),

    exportState: () => enqueue(() => wasm.export_game_state_json()),

    restoreState: (stateJson: string) =>
      enqueue(() => wasm.restore_game_state(stateJson)),

    resumeRestoredGameState: () =>
      enqueue(() => ({
        presentation: wasm.resume_restored_game_state() as RestoredStackAutomationPresentation,
        snapshot: {
          state: wasm.get_game_state() as GameState,
          legalResult: wasm.get_legal_actions_js() as LegalActionsResult,
        },
      })),

    resumeMultiplayerHostState: (stateJson: string) =>
      enqueue(() => ({
        presentation: wasm.resume_multiplayer_host_state(stateJson) as RestoredStackAutomationPresentation,
        snapshot: {
          state: wasm.get_game_state() as GameState,
          legalResult: wasm.get_legal_actions_js() as LegalActionsResult,
        },
      })),

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
        throwInitFailure(r);
        return { events: r.events ?? [], log_entries: r.log_entries ?? [] };
      }),

    initializeMultiplayerHostGame: (
      deckData: unknown | null,
      seed: number,
      formatConfig: FormatConfig | null,
      matchConfig: MatchConfig | null,
      playerCount?: number,
      firstPlayer?: number,
    ) =>
      enqueue(() => {
        const r = wasm.initialize_multiplayer_host_game(
          deckData,
          seed,
          formatConfig,
          matchConfig,
          playerCount ?? undefined,
          firstPlayer ?? undefined,
        );
        throwInitFailure(r);
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

    // The ENFORCING sibling — distinct binding, never the one above, so this
    // fallback cannot silently inherit the UI-hint path's Custom downgrade.
    evaluateDeckFormatGate: (request: unknown) =>
      enqueue(() => wasm.evaluateDeckFormatGate(request)),

    customFormatFromLobbyConfig: (name: string, formatConfig: unknown) =>
      enqueue(() => wasm.customFormatFromLobbyConfig(name, formatConfig)),

    formatConfigForCustomRules: (customRules: unknown) =>
      enqueue(() => wasm.formatConfigForCustomRules(customRules)),

    getCardFaceData: (cardName: string) =>
      enqueue(() => wasm.get_card_face_data(cardName)),

    getCardParseDetails: (cardName: string) =>
      enqueue(() => wasm.get_card_parse_details(cardName)),

    getCardRulings: (cardName: string) =>
      enqueue(() => wasm.get_card_rulings(cardName)),
  };
}
