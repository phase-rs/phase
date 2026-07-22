import { beforeEach, describe, expect, it, vi } from "vitest";

import { AiWorkerPool } from "../ai-worker-pool";

const mockWorkers = Array.from({ length: 2 }, () => ({
  initialize: vi.fn().mockResolvedValue(undefined),
  loadCardDbFromUrl: vi.fn().mockResolvedValue(100),
  restoreState: vi.fn().mockResolvedValue(undefined),
  getAiScoredCandidates: vi.fn(),
  dispose: vi.fn(),
}));

let workerIndex = 0;
let workerConstructorErrorAt: number | null = null;

vi.mock("../engine-worker-client", () => ({
  EngineWorkerClient: vi.fn().mockImplementation(function () {
    const currentWorkerIndex = workerIndex;
    workerIndex += 1;
    if (currentWorkerIndex === workerConstructorErrorAt) {
      throw new Error("worker construction failed");
    }
    return mockWorkers[currentWorkerIndex];
  }),
}));

describe("AiWorkerPool", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    workerIndex = 0;
    workerConstructorErrorAt = null;
  });

  it("disposes earlier workers when a later worker fails to construct", () => {
    workerConstructorErrorAt = 1;

    expect(() => new AiWorkerPool(2)).toThrow("worker construction failed");

    expect(mockWorkers[0].dispose).toHaveBeenCalledOnce();
    expect(mockWorkers[1].dispose).not.toHaveBeenCalled();
  });

  it("merges worker scores without reintroducing missing actions", async () => {
    const pass = { type: "PassPriority" } as const;
    const cast = {
      type: "CastSpell",
      data: { object_id: 7, card_id: 7, targets: [] },
    } as const;

    mockWorkers[0].getAiScoredCandidates.mockResolvedValue([
      [pass, 1.0],
      [cast, 2.0],
    ]);
    mockWorkers[1].getAiScoredCandidates.mockResolvedValue([[pass, 3.0]]);

    const pool = new AiWorkerPool(2);
    await pool.initialize();
    const merged = await pool.getAiScoredCandidates("{}", "VeryHard", 1);

    expect(merged).toEqual([
      [pass, 2.0],
      [cast, 2.0],
    ]);
  });

  it("does not invent an action absent from all worker results", async () => {
    const pass = { type: "PassPriority" } as const;

    mockWorkers[0].getAiScoredCandidates.mockResolvedValue([[pass, 1.0]]);
    mockWorkers[1].getAiScoredCandidates.mockResolvedValue([[pass, 3.0]]);

    const pool = new AiWorkerPool(2);
    await pool.initialize();
    const merged = await pool.getAiScoredCandidates("{}", "VeryHard", 1);

    expect(merged).toEqual([[pass, 2.0]]);
  });
});
