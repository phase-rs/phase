/**
 * Multi-worker AI pool for root parallelism.
 *
 * Each worker owns an independent WASM instance. For VeryHard difficulty,
 * all workers score candidates in parallel with different RNG seeds.
 * Results are merged by averaging scores per unique action, then the
 * engine worker performs softmax selection in Rust.
 *
 * Research backing: root parallelism has minimal communication overhead
 * (merge only at the end), scales near-linearly up to ~4 workers, and is
 * a natural fit for isolated Web Workers with no shared memory.
 */
import { EngineWorkerClient } from "./engine-worker-client";
import type { GameAction } from "./types";

export class AiWorkerPool {
  private workers: EngineWorkerClient[] = [];
  private cardDbLoaded = false;
  private currentGeneration = 0;

  // Serializes scoring rounds so restoreState+score sequences on each
  // worker cannot interleave when a new request arrives mid-computation.
  private scoringLock: Promise<void> = Promise.resolve();

  constructor(workerCount: number) {
    for (let i = 0; i < workerCount; i++) {
      this.workers.push(new EngineWorkerClient());
    }
  }

  async initialize(): Promise<void> {
    await Promise.all(this.workers.map((w) => w.initialize()));
  }

  /**
   * Each worker fetches card-data.json independently via loadCardDbFromUrl.
   * Browser/service worker cache (CacheFirst strategy) ensures a single
   * network request regardless of worker count.
   */
  async loadCardDb(): Promise<void> {
    await Promise.all(this.workers.map((w) => w.loadCardDbFromUrl()));
    this.cardDbLoaded = true;
  }

  /**
   * Load a pre-built game-scoped card-DB subset (small JSON) into every worker
   * instead of each fetching+parsing the full ~93MB corpus. Used by the AI pool
   * for bounded games to stay within the iOS WebKit memory ceiling.
   */
  async loadCardDbText(text: string): Promise<void> {
    await Promise.all(this.workers.map((w) => w.loadCardDb(text)));
    this.cardDbLoaded = true;
  }

  /**
   * Mark the pool's card DB stale so the next `ensureCardDb`/`ensureAiPool`
   * rebuilds this game's subset. The worker instances are preserved; only the
   * loaded-flag is flipped (the subset is game-scoped and must be rebuilt
   * per game).
   */
  invalidateCardDb(): void {
    this.cardDbLoaded = false;
  }

  get isCardDbLoaded(): boolean {
    return this.cardDbLoaded;
  }

  /**
   * Score candidates across all workers in parallel, merge results.
   * Returns null if the request was superseded (staleness guard).
   *
   * Scoring rounds are serialized via a lock to prevent interleaved
   * restoreState/score commands on the same worker from concurrent calls.
   */
  async getAiScoredCandidates(
    stateJson: string,
    difficulty: string,
    playerId: number,
  ): Promise<[GameAction, number][] | null> {
    const generation = ++this.currentGeneration;

    // Wait for any in-flight scoring round to finish before starting ours.
    // This prevents restoreState(B) from arriving at a worker between
    // restoreState(A) and scoreA, which would corrupt scoreA's state.
    await this.scoringLock;

    // Check if superseded while waiting for the lock
    if (this.currentGeneration !== generation) return null;

    let releaseLock!: () => void;
    this.scoringLock = new Promise((resolve) => {
      releaseLock = resolve;
    });

    try {
      // 1. Restore state in each worker (awaitable — ensures state
      //    is fully deserialized + rehydrated before scoring begins)
      await Promise.all(this.workers.map((w) => w.restoreState(stateJson)));

      // 2. Each worker computes scored candidates with different seed
      const baseSeed = Date.now();
      const results = await Promise.all(
        this.workers.map((w, i) =>
          w.getAiScoredCandidates(difficulty, playerId, baseSeed + i),
        ),
      );

      // 3. Check for staleness — if generation changed, discard results
      if (this.currentGeneration !== generation) return null;

      // 4. Merge scores: average per unique action
      return mergeScores(results);
    } finally {
      releaseLock();
    }
  }

  dispose(): void {
    this.workers.forEach((w) => w.dispose());
    this.workers = [];
  }
}

/**
 * Merge scored candidates from multiple workers by averaging scores per action.
 *
 * Uses JSON.stringify for action identity — safe because:
 * 1. GameAction uses serde's tag/content pattern → deterministic field order
 * 2. All values are integers (ObjectId, CardId) or strings — no floats
 * 3. Identical GameAction values always produce identical JSON strings
 */
function mergeScores(
  workerResults: [GameAction, number][][],
): [GameAction, number][] {
  const byKey = new Map<
    string,
    { action: GameAction; total: number; count: number }
  >();

  for (const results of workerResults) {
    for (const [action, score] of results) {
      const key = JSON.stringify(action);
      const entry = byKey.get(key) ?? { action, total: 0, count: 0 };
      entry.total += score;
      entry.count += 1;
      byKey.set(key, entry);
    }
  }

  return Array.from(byKey.values()).map(
    ({ action, total, count }) =>
      [action, total / count] as [GameAction, number],
  );
}
