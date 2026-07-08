/**
 * Promise-based RPC wrapper around the Engine Web Worker.
 *
 * All methods post a typed message to the worker with a unique request ID,
 * then resolve the corresponding promise when the worker responds.
 */
import type {
  BatchResolveResult,
  FormatConfig,
  GameAction,
  GameState,
  LegalActionsResult,
  MatchConfig,
  SubmitResult,
  ViewerSnapshot,
} from "./types";
import { AdapterError, AdapterErrorCode } from "./types";
import type { BracketDeckRequest, BracketEstimate } from "../types/bracketEstimate";
import { debugLog } from "../game/debugLog";
import { notifyEngineSlow } from "../game/engineRecovery";

type EngineResponse =
  | { type: "ready" }
  | { type: "result"; id: number; data: unknown }
  | { type: "error"; id: number; message: string; bracketViolation?: true };

/**
 * Watchdog timeout for gameplay round-trip calls. Generous on purpose: a
 * legitimately slow call (e.g. a turn-21 four-player board) can take many
 * seconds, and a false-positive timeout on a valid-but-slow call must not
 * kill the game. This does NOT speed anything up — it surfaces a recoverable
 * "still waiting" dialog while leaving the worker request alive. Tunable; only
 * applied to gameplay round-trips, never to bulk/long setup calls (card-DB
 * load, game init, batch resolve, restore).
 */
const ENGINE_REQUEST_TIMEOUT_MS = 60_000;

/**
 * Watchdog timeout for AI search round-trips (getAiAction /
 * getAiScoredCandidates / selectActionFromScores). Deliberately much larger
 * than ENGINE_REQUEST_TIMEOUT_MS: AI search legitimately exceeds 60s on
 * pathological boards (turn-40 squirrel / mana-token storms take hundreds of
 * seconds in debug; release is ~10-50x faster but can still cross a minute),
 * so the 60s gameplay window would false-positive and surface the engine-lost
 * recovery modal mid-AI-turn on a perfectly healthy worker. 5 minutes gives
 * generous headroom over realistic release AI times while still converting a
 * true infinite hang into a recoverable error.
 */
const ENGINE_AI_TIMEOUT_MS = 300_000;

export class EngineWorkerClient {
  private worker: Worker;
  private nextId = 0;
  private pending = new Map<
    number,
    {
      resolve: (value: unknown) => void;
      reject: (reason: Error) => void;
      timer?: ReturnType<typeof setTimeout>;
      slowNotified?: boolean;
    }
  >();
  private readyPromise: Promise<void>;
  private readyResolve!: () => void;

  constructor() {
    this.worker = new Worker(
      new URL("./engine-worker.ts", import.meta.url),
      { type: "module" },
    );

    this.readyPromise = new Promise<void>((resolve) => {
      this.readyResolve = resolve;
    });

    this.worker.onmessage = (e: MessageEvent<EngineResponse>) => {
      const msg = e.data;
      switch (msg.type) {
        case "ready":
          this.readyResolve();
          break;
        case "result": {
          const entry = this.pending.get(msg.id);
          if (entry) {
            this.pending.delete(msg.id);
            if (entry.timer) clearTimeout(entry.timer);
            entry.resolve(msg.data);
          }
          break;
        }
        case "error": {
          const entry = this.pending.get(msg.id);
          if (entry) {
            this.pending.delete(msg.id);
            if (entry.timer) clearTimeout(entry.timer);
            // Bracket violation is a typed rejection so the caller can match
            // by code rather than by string substring on the error message.
            const err = msg.bracketViolation
              ? new AdapterError(AdapterErrorCode.BRACKET_VIOLATION, msg.message, false)
              : new Error(msg.message);
            entry.reject(err);
          }
          break;
        }
      }
    };

    this.worker.onerror = (e: ErrorEvent) => {
      // Reject all pending requests — log via debugLog for in-app visibility
      const msg = e.message ?? "Worker error";
      debugLog(`Engine worker error: ${msg} (${this.pending.size} pending requests rejected)`);
      for (const [, entry] of this.pending) {
        if (entry.timer) clearTimeout(entry.timer);
        entry.reject(new Error(msg));
      }
      this.pending.clear();
    };
  }

  /**
   * Post a typed message to the worker and resolve when it replies.
   *
   * `timeoutMs` arms a watchdog: if the worker doesn't reply within the
   * window, the pending entry stays alive and the UI is notified that the
   * request is slow. This is applied ONLY to gameplay round-trips (see call
   * sites below) — never to bulk/long setup calls (card-DB load, game init,
   * batch resolve, restore), where a long runtime is expected. A late worker
   * reply still resolves the original promise and clears the dispatch mutex.
   */
  private request<T>(message: Record<string, unknown>, timeoutMs?: number): Promise<T> {
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      const timer =
        timeoutMs !== undefined
          ? setTimeout(() => {
              const entry = this.pending.get(id);
              if (entry && !entry.slowNotified) {
                entry.slowNotified = true;
                notifyEngineSlow(`${String(message.type)}-timeout`);
              }
            }, timeoutMs)
          : undefined;
      this.pending.set(id, {
        resolve: resolve as (value: unknown) => void,
        reject,
        timer,
      });
      this.worker.postMessage({ ...message, id });
    });
  }

  async initialize(): Promise<void> {
    this.worker.postMessage({ type: "init" });
    await this.readyPromise;
  }

  async loadCardDb(text: string): Promise<number> {
    return this.request<number>({ type: "loadCardDb", cardDataText: text });
  }

  async loadCardDbFromUrl(): Promise<number> {
    return this.request<number>({ type: "loadCardDbFromUrl" });
  }

  async evaluateDeckCompatibility(request: unknown): Promise<unknown> {
    return this.request<unknown>({ type: "evaluateDeckCompatibility", request });
  }

  /**
   * Build the game-scoped AI card-DB subset for THIS game. Returns the
   * serialized `AiCardSubsetResult` tagged union (parse with `JSON.parse`).
   * Called ONLY on the MAIN engine client (full CARD_DB + live GAME_STATE).
   */
  async buildAiCardSubset(): Promise<string> {
    return this.request<string>({ type: "buildAiCardSubset" });
  }

  async initializeGame(
    deckData: unknown | null,
    seed: number,
    formatConfig: FormatConfig | null,
    matchConfig: MatchConfig | null,
    playerCount?: number,
    firstPlayer?: number,
  ): Promise<SubmitResult> {
    return this.request<SubmitResult>({
      type: "initializeGame",
      deckData,
      seed,
      formatConfig,
      matchConfig,
      playerCount,
      firstPlayer,
    });
  }

  // ── Gameplay round-trips ──────────────────────────────────────────────
  // Each of these is a per-action engine call that the UI awaits before it can
  // continue (and that holds the dispatch mutex). They carry a watchdog that
  // surfaces a "still waiting" prompt after ENGINE_REQUEST_TIMEOUT_MS without
  // cancelling the underlying worker request. Human round-trips use 60s;
  // the AI-search getters (getAiAction / getAiScoredCandidates /
  // selectActionFromScores) use the far longer ENGINE_AI_TIMEOUT_MS because a
  // healthy search can legitimately exceed a minute on pathological boards.
  // Bulk/long setup calls (card-DB load, game init, deck compatibility, batch
  // resolve, restore/resume, export, bracket estimate) deliberately omit the
  // timeout — their runtime is legitimately long.

  async submitAction(actor: number, action: GameAction): Promise<SubmitResult> {
    return this.request<SubmitResult>(
      { type: "submitAction", actor, action },
      ENGINE_REQUEST_TIMEOUT_MS,
    );
  }

  async getState(): Promise<GameState> {
    return this.request<GameState>({ type: "getState" }, ENGINE_REQUEST_TIMEOUT_MS);
  }

  async getFilteredState(viewerId: number): Promise<GameState> {
    return this.request<GameState>(
      { type: "getFilteredState", viewerId },
      ENGINE_REQUEST_TIMEOUT_MS,
    );
  }

  async getLegalActions(): Promise<LegalActionsResult> {
    return this.request<LegalActionsResult>(
      { type: "getLegalActions" },
      ENGINE_REQUEST_TIMEOUT_MS,
    );
  }

  async getLegalActionsForViewer(viewerId: number): Promise<LegalActionsResult> {
    return this.request<LegalActionsResult>(
      { type: "getLegalActionsForViewer", viewerId },
      ENGINE_REQUEST_TIMEOUT_MS,
    );
  }

  async getViewerSnapshot(viewerId: number): Promise<ViewerSnapshot> {
    return this.request<ViewerSnapshot>(
      { type: "getViewerSnapshot", viewerId },
      ENGINE_REQUEST_TIMEOUT_MS,
    );
  }

  async getAiAction(
    difficulty: string,
    playerId: number,
  ): Promise<GameAction | null> {
    return this.request<GameAction | null>(
      {
        type: "getAiAction",
        difficulty,
        playerId,
      },
      ENGINE_AI_TIMEOUT_MS,
    );
  }

  async getAiScoredCandidates(
    difficulty: string,
    playerId: number,
    seed: number,
  ): Promise<[GameAction, number][]> {
    return this.request<[GameAction, number][]>(
      {
        type: "getAiScoredCandidates",
        difficulty,
        playerId,
        seed,
      },
      ENGINE_AI_TIMEOUT_MS,
    );
  }

  async selectActionFromScores(
    scoresJson: string,
    difficulty: string,
    seed: number,
  ): Promise<GameAction | null> {
    return this.request<GameAction | null>(
      {
        type: "selectActionFromScores",
        scoresJson,
        difficulty,
        seed,
      },
      ENGINE_AI_TIMEOUT_MS,
    );
  }

  async exportState(): Promise<string> {
    return this.request<string>({ type: "exportState" });
  }

  async restoreState(stateJson: string): Promise<void> {
    await this.request<null>({ type: "restoreState", stateJson });
  }

  /**
   * Host-resume entry point. Unlike `restoreState` (undo semantics, stale
   * RNG seed, refused when multiplayer is already on), this loads a
   * persisted multiplayer-host state with a fresh RNG seed and atomically
   * flips the engine's multiplayer flag. Mirrors server-core's
   * `GameSession::from_persisted`.
   */
  async resumeMultiplayerHostState(stateJson: string): Promise<void> {
    await this.request<null>({ type: "resumeMultiplayerHostState", stateJson });
  }

  async resetGame(): Promise<void> {
    await this.request<null>({ type: "resetGame" });
  }

  async setMultiplayerMode(enabled: boolean): Promise<void> {
    await this.request<null>({ type: "setMultiplayerMode", enabled });
  }

  // Fast multiplayer-host seat-projection round-trips (pure state transforms,
  // no AI search or animation). Intentionally left without the gameplay
  // watchdog: they don't hold the dispatch mutex and a wedge here surfaces
  // through the host's own connection/recovery path rather than the per-action
  // recovery prompt.
  async applySeatMutation(stateJson: string, mutationJson: string): Promise<unknown> {
    return this.request<unknown>({
      type: "applySeatMutation",
      stateJson,
      mutationJson,
    });
  }

  async projectSeatView(stateJson: string): Promise<unknown> {
    return this.request<unknown>({
      type: "projectSeatView",
      stateJson,
    });
  }

  async resolveAll(
    requester: number,
    aiSeats: { playerId: number; difficulty: string }[],
    maxResolutions: number = 0,
  ): Promise<BatchResolveResult> {
    // Intentionally no watchdog timeout: a batch resolve can be legitimately
    // very long (a multi-thousand-item storm draining one chunk at a time),
    // and a false timeout mid-drain is worse than a long wait. Residual risk:
    // if the worker wedges *inside* resolveAll itself the promise never settles
    // and the "Resolving…" overlay sticks. Accepted as a lower-severity UX
    // bug than false-positiving a healthy long drain — revisit only if a
    // bounded per-chunk timeout proves safe.
    return this.request<BatchResolveResult>({
      type: "resolveAll",
      requester,
      aiSeatsJson: JSON.stringify(aiSeats),
      maxResolutions,
    });
  }

  async ping(): Promise<string> {
    return this.request<string>({ type: "ping" });
  }

  /**
   * Drain the panic message captured by the Rust panic hook in engine-wasm.
   * Returns `null` if no panic has been observed since the last drain.
   *
   * The adapter calls this after a thrown STATE_LOST sentinel: if a panic
   * is present, the failure is a real engine crash (re-running the same
   * input will re-panic) and recovery must surface it instead of retrying.
   */
  async takeLastPanic(): Promise<string | null> {
    return this.request<string | null>({ type: "takeLastPanic" });
  }

  async estimateBracketForDeck(deck: BracketDeckRequest): Promise<BracketEstimate | null> {
    return this.request<BracketEstimate | null>({ type: "estimateBracketForDeck", deck });
  }

  dispose(): void {
    for (const [, entry] of this.pending) {
      if (entry.timer) clearTimeout(entry.timer);
      entry.reject(new Error("Worker disposed"));
    }
    this.pending.clear();
    this.worker.terminate();
  }
}
