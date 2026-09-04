/**
 * Promise-based RPC wrapper around the Engine Web Worker.
 *
 * All methods post a typed message to the worker with a unique request ID,
 * then resolve the corresponding promise when the worker responds.
 */
import type {
  AiActionProposal,
  AiDecisionDiagnosticReceipt,
  AiProposalSubmission,
  FormatConfig,
  GameAction,
  GameState,
  LegalActionsResult,
  MatchConfig,
  ReplayHeader,
  RestoredStackAutomationPresentation,
  SubmitResult,
  ViewerSnapshot,
} from "./types";
import {
  actionRejectionError,
  AdapterError,
  AdapterErrorCode,
  isActionRejection,
} from "./types";
import type { InteractionSubmission } from "./generated/interaction";
import type { BracketDeckRequest, BracketEstimate } from "../types/bracketEstimate";
import { debugLog } from "../game/debugLog";
import { notifyEngineSlow } from "../game/engineRecovery";

type EngineResponse =
  | { type: "result"; id: number; data: unknown }
  | {
      type: "error";
      id: number;
      message: string;
      bracketViolation?: true;
      engineOccupied?: true;
      actionRejection?: unknown;
    };

type RestoredWorkerResult = {
  presentation: RestoredStackAutomationPresentation;
  snapshot: { state: GameState; legalResult: LegalActionsResult };
};

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
 * Hard deadline for the initial WASM worker handshake. Unlike gameplay
 * watchdogs, initialization has no useful late-response path: rejecting lets
 * WasmAdapter dispose the stalled worker and activate its main-thread fallback.
 */
const ENGINE_INITIALIZATION_TIMEOUT_MS = 30_000;
const MALFORMED_ACTION_REJECTION_MESSAGE = "The engine rejected that action.";

type RequestTimeoutBehavior = "notify" | "reject";

/**
 * Watchdog timeout for AI proposal generation. Deliberately much larger
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
  constructor() {
    this.worker = new Worker(
      new URL("./engine-worker.ts", import.meta.url),
      { type: "module" },
    );

    this.worker.onmessage = (e: MessageEvent<EngineResponse>) => {
      const msg = e.data;
      switch (msg.type) {
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
            // Bracket violation and occupied-engine refusals are typed
            // rejections so the caller can match by code rather than by string
            // substring on the error message.
            let err: Error;
            if ("actionRejection" in msg && isActionRejection(msg.actionRejection)) {
              err = actionRejectionError(msg.actionRejection);
            } else if (
              msg.actionRejection !== undefined
              || (
                "actionRejection" in msg
                && msg.message === MALFORMED_ACTION_REJECTION_MESSAGE
              )
            ) {
              err = new AdapterError(
                AdapterErrorCode.ACTION_REJECTED,
                MALFORMED_ACTION_REJECTION_MESSAGE,
                true,
              );
            } else if (msg.bracketViolation) {
              err = new AdapterError(AdapterErrorCode.BRACKET_VIOLATION, msg.message, false);
            } else if (msg.engineOccupied) {
              err = new AdapterError(AdapterErrorCode.ENGINE_OCCUPIED, msg.message, false);
            } else {
              err = new Error(msg.message);
            }
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
   * `timeoutMs` arms a watchdog. The default `notify` behavior keeps a slow
   * gameplay request alive and informs the UI, allowing a late reply to resolve
   * normally. The initialization-only `reject` behavior removes and rejects a
   * stalled request so the adapter can fall back. Bulk setup calls (card-DB
   * load, game init, batch resolve, restore) deliberately have no timeout.
   */
  private request<T>(
    message: Record<string, unknown>,
    timeoutMs?: number,
    timeoutBehavior: RequestTimeoutBehavior = "notify",
  ): Promise<T> {
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      const timer =
        timeoutMs !== undefined
          ? setTimeout(() => {
              const entry = this.pending.get(id);
              if (!entry) return;
              if (timeoutBehavior === "reject") {
                this.pending.delete(id);
                entry.reject(
                  new Error(
                    `Engine worker ${String(message.type)} timed out after ${timeoutMs}ms`,
                  ),
                );
              } else if (!entry.slowNotified) {
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
    await this.request<null>(
      { type: "init" },
      ENGINE_INITIALIZATION_TIMEOUT_MS,
      "reject",
    );
  }

  async loadCardDb(text: string): Promise<number> {
    return this.request<number>({ type: "loadCardDb", cardDataText: text });
  }

  async loadCardDbFromUrl(): Promise<number> {
    return this.request<number>({ type: "loadCardDbFromUrl" });
  }

  async buildAiCardSubset(): Promise<string> {
    return this.request<string>({ type: "buildAiCardSubset" });
  }

  async evaluateDeckCompatibility(request: unknown): Promise<unknown> {
    return this.request<unknown>({ type: "evaluateDeckCompatibility", request });
  }

  /** Always-definite deck/format verdict for ENFORCING callers. See
   *  `WasmAdapter.evaluateDeckFormatGate`. */
  async evaluateDeckFormatGate(request: unknown): Promise<unknown> {
    return this.request<unknown>({ type: "evaluateDeckFormatGate", request });
  }

  async customFormatFromLobbyConfig(name: string, formatConfig: unknown): Promise<unknown> {
    return this.request<unknown>({ type: "customFormatFromLobbyConfig", name, formatConfig });
  }

  async formatConfigForCustomRules(customRules: unknown): Promise<unknown> {
    return this.request<unknown>({ type: "formatConfigForCustomRules", customRules });
  }

  async getCardFaceData(cardName: string): Promise<unknown> {
    return this.request<unknown>({ type: "getCardFaceData", cardName });
  }

  async getCardParseDetails(cardName: string): Promise<unknown> {
    return this.request<unknown>({ type: "getCardParseDetails", cardName });
  }

  async getCardRulings(cardName: string): Promise<unknown> {
    return this.request<unknown>({ type: "getCardRulings", cardName });
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

  /**
   * Host-start entry point. Unlike `initializeGame`, the engine refuses when it
   * already holds a game and claims the multiplayer flag in the same call that
   * installs the state — so a host sharing this worker with local play can
   * never overwrite (or be overwritten by) the other session. Rejects with
   * `AdapterErrorCode.ENGINE_OCCUPIED` on refusal.
   */
  async initializeMultiplayerHostGame(
    deckData: unknown | null,
    seed: number,
    formatConfig: FormatConfig | null,
    matchConfig: MatchConfig | null,
    playerCount?: number,
    firstPlayer?: number,
  ): Promise<SubmitResult> {
    return this.request<SubmitResult>({
      type: "initializeMultiplayerHostGame",
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
  // AI proposal generation uses the far longer ENGINE_AI_TIMEOUT_MS because a
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

  async submitInteraction(actor: number, submission: InteractionSubmission): Promise<SubmitResult> {
    return this.request<SubmitResult>(
      { type: "submitInteraction", actor, submission },
      ENGINE_REQUEST_TIMEOUT_MS,
    );
  }

  async previewManaPayment(actor: number, action: GameAction): Promise<number[]> {
    return this.request<number[]>(
      { type: "previewManaPayment", actor, action },
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

  /**
   * Atomic state + legal-actions read. The worker services this as one
   * synchronous block, so the pair can never straddle an engine advance.
   * Same timeout class as `getState`. The caller (`WasmAdapter.getSnapshot`)
   * stamps the `seq` on arrival.
   */
  async getSnapshot(): Promise<{ state: GameState; legalResult: LegalActionsResult }> {
    return this.request<{ state: GameState; legalResult: LegalActionsResult }>(
      { type: "getSnapshot" },
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

  async getAiActionProposal(
    difficulty: string,
    playerId: number,
  ): Promise<AiActionProposal | null> {
    return this.request<AiActionProposal | null>(
      { type: "getAiActionProposal", difficulty, playerId },
      ENGINE_AI_TIMEOUT_MS,
    );
  }

  async getAiActionProposalWithDiagnostics(
    difficulty: string,
    playerId: number,
  ): Promise<{ proposal: AiActionProposal; receipt: AiDecisionDiagnosticReceipt } | null> {
    return this.request(
      { type: "getAiActionProposalWithDiagnostics", difficulty, playerId },
      ENGINE_AI_TIMEOUT_MS,
    );
  }

  /** Engine-owned tactical floor for a decision whose optional scorer timed out. */
  async getAiTacticalActionProposal(
    difficulty: string,
    playerId: number,
  ): Promise<AiActionProposal | null> {
    return this.request<AiActionProposal | null>(
      { type: "getAiTacticalActionProposal", difficulty, playerId },
      ENGINE_REQUEST_TIMEOUT_MS,
    );
  }

  async getAiTacticalActionProposalWithDiagnostics(
    difficulty: string,
    playerId: number,
  ): Promise<{ proposal: AiActionProposal; receipt: AiDecisionDiagnosticReceipt } | null> {
    return this.request(
      { type: "getAiTacticalActionProposalWithDiagnostics", difficulty, playerId },
      ENGINE_REQUEST_TIMEOUT_MS,
    );
  }

  /** This worker-side endpoint scores only; it cannot mint a proposal. */
  async getAiScoredCandidates(
    difficulty: string,
    playerId: number,
    seed: number,
  ): Promise<[GameAction, number][]> {
    return this.request<[GameAction, number][]>(
      { type: "getAiScoredCandidates", difficulty, playerId, seed },
      ENGINE_AI_TIMEOUT_MS,
    );
  }

  /** Main authority filters scores through a fresh contract before minting. */
  async getAiActionProposalFromScores(
    scoresJson: string,
    difficulty: string,
    playerId: number,
    seed: number,
  ): Promise<AiActionProposal | null> {
    return this.request<AiActionProposal | null>(
      { type: "getAiActionProposalFromScores", scoresJson, difficulty, playerId, seed },
      ENGINE_AI_TIMEOUT_MS,
    );
  }

  async getAiActionProposalFromScoresWithDiagnostics(
    scoresJson: string,
    difficulty: string,
    playerId: number,
    seed: number,
  ): Promise<{ proposal: AiActionProposal; receipt: AiDecisionDiagnosticReceipt } | null> {
    return this.request(
      { type: "getAiActionProposalFromScoresWithDiagnostics", scoresJson, difficulty, playerId, seed },
      ENGINE_AI_TIMEOUT_MS,
    );
  }

  async submitAiActionProposal(
    proposal: AiActionProposal,
  ): Promise<AiProposalSubmission> {
    return this.request<AiProposalSubmission>(
      { type: "submitAiActionProposal", proposal },
      ENGINE_REQUEST_TIMEOUT_MS,
    );
  }

  async exportState(): Promise<string> {
    return this.request<string>({ type: "exportState" });
  }

  async restoreState(stateJson: string): Promise<void> {
    await this.request<null>({ type: "restoreState", stateJson });
  }

  async resumeRestoredGameState(): Promise<RestoredWorkerResult> {
    return this.request<RestoredWorkerResult>({ type: "resumeRestoredGameState" });
  }

  /**
   * Host-resume entry point. Unlike `restoreState` (undo semantics, stale
   * RNG seed, refused when multiplayer is already on), this loads a
   * persisted multiplayer-host state with a fresh RNG seed and atomically
   * flips the engine's multiplayer flag. Mirrors server-core's
   * `GameSession::from_persisted`.
   */
  async resumeMultiplayerHostState(stateJson: string): Promise<RestoredWorkerResult> {
    return this.request<RestoredWorkerResult>({ type: "resumeMultiplayerHostState", stateJson });
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

  // ── Replay system ──────────────────────────────────────────────────────
  // Recording (hasReplayRecording / exportReplayLog) reads the in-progress
  // recording WASM auto-starts alongside the live game. Playback
  // (loadReplayForPlayback / replaySeek / replayLength / replayHeader /
  // clearReplayPlayback) is independent of the live game entirely — a
  // `ReplayAdapter` typically owns its own `EngineWorkerClient` instance so
  // viewing a replay never touches an in-progress game's worker.

  async hasReplayRecording(): Promise<boolean> {
    return this.request<boolean>({ type: "hasReplayRecording" });
  }

  /** Serialize the current game's replay recording, suitable for downloading. */
  async exportReplayLog(): Promise<string> {
    return this.request<string>({ type: "exportReplayLog" });
  }

  /** Load a replay (the JSON `exportReplayLog` produced) for scrubbing. Returns the recorded action count. */
  async loadReplayForPlayback(replayJson: string): Promise<number> {
    return this.request<number>({ type: "loadReplayForPlayback", replayJson });
  }

  async replayLength(): Promise<number> {
    return this.request<number>({ type: "replayLength" });
  }

  async replayHeader(): Promise<ReplayHeader | null> {
    return this.request<ReplayHeader | null>({ type: "replayHeader" });
  }

  /**
   * Seek the loaded replay to `target` (clamped to its length). Returns the
   * raw `{ state, derived }` wire envelope (or `null`) — same shape
   * `get_game_state` returns — so callers unwrap it the same way
   * `wasm-adapter.ts`'s `unwrapClientGameState` does for live games.
   */
  async replaySeek(target: number): Promise<unknown> {
    return this.request<unknown>({ type: "replaySeek", target });
  }

  async clearReplayPlayback(): Promise<void> {
    await this.request<null>({ type: "clearReplayPlayback" });
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
