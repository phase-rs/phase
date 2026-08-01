/**
 * Parallel AI scoring workers. Workers only return serializable candidate
 * scores; the authoritative engine is the only process that can turn a score
 * into a capability-backed proposal.
 */
import { EngineWorkerClient } from "./engine-worker-client";
import type { GameAction } from "./types";

export class AiWorkerPool {
  private workers: EngineWorkerClient[] = [];
  private cardDbLoaded = false;
  private currentGeneration = 0;
  private scoringLock: Promise<void> = Promise.resolve();

  constructor(workerCount: number) {
    try {
      for (let index = 0; index < workerCount; index += 1) {
        this.workers.push(new EngineWorkerClient());
      }
    } catch (error) {
      this.dispose();
      throw error;
    }
  }

  async initialize(): Promise<void> {
    await Promise.all(this.workers.map((worker) => worker.initialize()));
  }

  async loadCardDb(): Promise<void> {
    await Promise.all(this.workers.map((worker) => worker.loadCardDbFromUrl()));
    this.cardDbLoaded = true;
  }

  async loadCardDbText(text: string): Promise<void> {
    await Promise.all(this.workers.map((worker) => worker.loadCardDb(text)));
    this.cardDbLoaded = true;
  }

  invalidateCardDb(): void {
    this.cardDbLoaded = false;
  }

  get isCardDbLoaded(): boolean {
    return this.cardDbLoaded;
  }

  /**
   * Scores restored state in isolated workers. The returned actions are not
   * capabilities and cannot be submitted anywhere; callers must rebind them
   * through the main engine's proposal issuer.
   */
  async getAiScoredCandidates(
    stateJson: string,
    difficulty: string,
    playerId: number,
  ): Promise<[GameAction, number][] | null> {
    const generation = ++this.currentGeneration;
    await this.scoringLock;
    if (this.currentGeneration !== generation) return null;

    let releaseLock!: () => void;
    this.scoringLock = new Promise((resolve) => {
      releaseLock = resolve;
    });

    try {
      await Promise.all(this.workers.map((worker) => worker.restoreState(stateJson)));
      const baseSeed = Date.now();
      const scores = await Promise.all(
        this.workers.map((worker, index) =>
          worker.getAiScoredCandidates(difficulty, playerId, baseSeed + index),
        ),
      );
      return this.currentGeneration === generation ? mergeScores(scores) : null;
    } finally {
      releaseLock();
    }
  }

  dispose(): void {
    this.workers.forEach((worker) => worker.dispose());
    this.workers = [];
  }
}

function mergeScores(workerResults: [GameAction, number][][]): [GameAction, number][] {
  const byAction = new Map<string, { action: GameAction; total: number; count: number }>();
  for (const scores of workerResults) {
    for (const [action, score] of scores) {
      const key = JSON.stringify(action);
      const entry = byAction.get(key) ?? { action, total: 0, count: 0 };
      entry.total += score;
      entry.count += 1;
      byAction.set(key, entry);
    }
  }
  return Array.from(byAction.values()).map(({ action, total, count }) => [action, total / count]);
}
