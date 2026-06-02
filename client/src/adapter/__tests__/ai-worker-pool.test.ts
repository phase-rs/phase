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

vi.mock("../engine-worker-client", () => ({
  EngineWorkerClient: vi.fn().mockImplementation(function () {
    const worker = mockWorkers[workerIndex];
    workerIndex += 1;
    return worker;
  }),
}));

describe("AiWorkerPool", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    workerIndex = 0;
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
