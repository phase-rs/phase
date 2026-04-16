/**
 * Promise-based RPC wrapper around the Engine Web Worker.
 *
 * All methods post a typed message to the worker with a unique request ID,
 * then resolve the corresponding promise when the worker responds.
 */
import type {
  FormatConfig,
  GameAction,
  GameState,
  LegalActionsResult,
  MatchConfig,
  SubmitResult,
} from "./types";
import { debugLog } from "../game/debugLog";

type EngineResponse =
  | { type: "ready" }
  | { type: "result"; id: number; data: unknown }
  | { type: "error"; id: number; message: string };

export class EngineWorkerClient {
  private worker: Worker;
  private nextId = 0;
  private pending = new Map<
    number,
    { resolve: (value: unknown) => void; reject: (reason: Error) => void }
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
            entry.resolve(msg.data);
          }
          break;
        }
        case "error": {
          const entry = this.pending.get(msg.id);
          if (entry) {
            this.pending.delete(msg.id);
            entry.reject(new Error(msg.message));
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
        entry.reject(new Error(msg));
      }
      this.pending.clear();
    };
  }

  private request<T>(message: Record<string, unknown>): Promise<T> {
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, {
        resolve: resolve as (value: unknown) => void,
        reject,
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

  async submitAction(actor: number, action: GameAction): Promise<SubmitResult> {
    return this.request<SubmitResult>({ type: "submitAction", actor, action });
  }

  async getState(): Promise<GameState> {
    return this.request<GameState>({ type: "getState" });
  }

  async getFilteredState(viewerId: number): Promise<GameState> {
    return this.request<GameState>({ type: "getFilteredState", viewerId });
  }

  async getLegalActions(): Promise<LegalActionsResult> {
    return this.request<LegalActionsResult>({ type: "getLegalActions" });
  }

  async getAiAction(
    difficulty: string,
    playerId: number,
  ): Promise<GameAction | null> {
    return this.request<GameAction | null>({
      type: "getAiAction",
      difficulty,
      playerId,
    });
  }

  async getAiScoredCandidates(
    difficulty: string,
    playerId: number,
    seed: number,
  ): Promise<[GameAction, number][]> {
    return this.request<[GameAction, number][]>({
      type: "getAiScoredCandidates",
      difficulty,
      playerId,
      seed,
    });
  }

  async selectActionFromScores(
    scoresJson: string,
    difficulty: string,
    seed: number,
  ): Promise<GameAction | null> {
    return this.request<GameAction | null>({
      type: "selectActionFromScores",
      scoresJson,
      difficulty,
      seed,
    });
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

  async applySeatMutation(stateJson: string, mutationJson: string): Promise<unknown> {
    return this.request<unknown>({
      type: "applySeatMutation",
      stateJson,
      mutationJson,
    });
  }

  async ping(): Promise<string> {
    return this.request<string>({ type: "ping" });
  }

  dispose(): void {
    for (const [, entry] of this.pending) {
      entry.reject(new Error("Worker disposed"));
    }
    this.pending.clear();
    this.worker.terminate();
  }
}
