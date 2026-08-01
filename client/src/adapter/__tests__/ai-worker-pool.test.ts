import { beforeEach, describe, expect, it, vi } from "vitest";

import { AiWorkerPool } from "../ai-worker-pool";

const workerHarness = vi.hoisted(() => ({
  workers: Array.from({ length: 2 }, () => ({
    initialize: vi.fn().mockResolvedValue(undefined),
    loadCardDb: vi.fn().mockResolvedValue(1),
    loadCardDbFromUrl: vi.fn().mockResolvedValue(1),
    restoreState: vi.fn().mockResolvedValue(undefined),
    getAiScoredCandidates: vi.fn(),
    dispose: vi.fn(),
  })),
  nextWorker: 0,
}));
const { workers } = workerHarness;

vi.mock("../engine-worker-client", () => ({
  EngineWorkerClient: class {
    constructor() {
      return workerHarness.workers[workerHarness.nextWorker++];
    }
  },
}));

describe("AiWorkerPool", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    workerHarness.nextWorker = 0;
  });

  it("merges score-only candidates without minting or dispatching an action", async () => {
    const pass = { type: "PassPriority" } as const;
    workers[0].getAiScoredCandidates.mockResolvedValue([[pass, 1]]);
    workers[1].getAiScoredCandidates.mockResolvedValue([[pass, 3]]);

    const pool = new AiWorkerPool(2);
    await pool.initialize();

    expect(await pool.getAiScoredCandidates("{}", "VeryHard", 1)).toEqual([[pass, 2]]);
  });

  it("invalidates score batches superseded while awaiting the pool lock", async () => {
    let completeFirst!: () => void;
    const first = new Promise<void>((resolve) => {
      completeFirst = resolve;
    });
    workers[0].restoreState.mockReturnValueOnce(first);
    workers[0].getAiScoredCandidates.mockResolvedValue([]);
    workers[1].getAiScoredCandidates.mockResolvedValue([]);
    const pool = new AiWorkerPool(2);

    const stale = pool.getAiScoredCandidates("old", "VeryHard", 0);
    const current = pool.getAiScoredCandidates("current", "VeryHard", 0);
    completeFirst();

    await expect(stale).resolves.toBeNull();
    await expect(current).resolves.toEqual([]);
  });
});
