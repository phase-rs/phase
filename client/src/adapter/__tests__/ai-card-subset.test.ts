import { beforeEach, describe, expect, it, vi } from "vitest";

import { loadAiPoolCardDb } from "../card-db-subset";
import type { EngineWorkerClient } from "../engine-worker-client";
import type { AiWorkerPool } from "../ai-worker-pool";

// The mocked EngineWorkerClient is shared by both the main engine and every AI
// pool worker (the pool constructs `new EngineWorkerClient()`), so a single
// mock records the text loaded into pool workers via `loadCardDb(text)`.
const mockWorkerClient = {
  initialize: vi.fn().mockResolvedValue(undefined),
  loadCardDb: vi.fn().mockResolvedValue(100),
  loadCardDbFromUrl: vi.fn().mockResolvedValue(100),
  buildAiCardSubset: vi.fn<() => Promise<string>>(),
  exportState: vi.fn().mockResolvedValue("{}"),
  restoreState: vi.fn().mockResolvedValue(undefined),
  getAiScoredCandidates: vi
    .fn()
    .mockResolvedValue([[{ type: "PassPriority" }, 1.0]]),
  getAiAction: vi.fn().mockResolvedValue(null),
  selectActionFromScores: vi.fn().mockResolvedValue({ type: "PassPriority" }),
  resetGame: vi.fn().mockResolvedValue(undefined),
  takeLastPanic: vi.fn().mockResolvedValue(null),
  dispose: vi.fn(),
};

vi.mock("../engine-worker-client", () => ({
  EngineWorkerClient: vi.fn().mockImplementation(function () {
    return mockWorkerClient;
  }),
}));

// ── VM-4: loadAiPoolCardDb dispatch ───────────────────────────────────────
describe("loadAiPoolCardDb", () => {
  function makeMocks() {
    const mainEngine = {
      buildAiCardSubset: vi.fn<() => Promise<string>>(),
    } as unknown as EngineWorkerClient & {
      buildAiCardSubset: ReturnType<typeof vi.fn>;
    };
    const aiPool = {
      loadCardDb: vi.fn().mockResolvedValue(undefined),
      loadCardDbText: vi.fn().mockResolvedValue(undefined),
    } as unknown as AiWorkerPool & {
      loadCardDb: ReturnType<typeof vi.fn>;
      loadCardDbText: ReturnType<typeof vi.fn>;
    };
    return { mainEngine, aiPool };
  }

  it("escalates a Momir game (kind:full) to the full DB, never the subset", async () => {
    const { mainEngine, aiPool } = makeMocks();
    mainEngine.buildAiCardSubset.mockResolvedValue(JSON.stringify({ kind: "full" }));

    await loadAiPoolCardDb("subset", mainEngine, aiPool);

    expect(aiPool.loadCardDb).toHaveBeenCalledOnce();
    expect(aiPool.loadCardDbText).not.toHaveBeenCalled();
  });

  it("loads the subset JSON for a bounded (non-Momir) game", async () => {
    const { mainEngine, aiPool } = makeMocks();
    const innerJson = '{"Bounded Card":{}}';
    mainEngine.buildAiCardSubset.mockResolvedValue(
      JSON.stringify({ kind: "subset", json: innerJson, count: 1 }),
    );

    await loadAiPoolCardDb("subset", mainEngine, aiPool);

    expect(aiPool.loadCardDbText).toHaveBeenCalledWith(innerJson);
    expect(aiPool.loadCardDb).not.toHaveBeenCalled();
  });

  it("mode=full loads the full DB without consulting the engine", async () => {
    const { mainEngine, aiPool } = makeMocks();

    await loadAiPoolCardDb("full", mainEngine, aiPool);

    expect(aiPool.loadCardDb).toHaveBeenCalledOnce();
    expect(mainEngine.buildAiCardSubset).not.toHaveBeenCalled();
    expect(aiPool.loadCardDbText).not.toHaveBeenCalled();
  });
});

// ── VM-1: cross-game subset invalidation + rebuild ────────────────────────
describe("WasmAdapter AI-pool subset lifecycle", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockWorkerClient.getAiScoredCandidates.mockResolvedValue([
      [{ type: "PassPriority" }, 1.0],
    ]);
  });

  it("rebuilds the pool's game-scoped subset after resetGameState (no cross-game leak)", async () => {
    const { WasmAdapter } = await import("../wasm-adapter");

    const subsetA = JSON.stringify({
      kind: "subset",
      json: '{"Game A Card":{}}',
      count: 1,
    });
    const subsetB = JSON.stringify({
      kind: "subset",
      json: '{"Game B Card":{}}',
      count: 1,
    });
    mockWorkerClient.buildAiCardSubset
      .mockResolvedValueOnce(subsetA)
      .mockResolvedValueOnce(subsetB);

    const adapter = new WasmAdapter();
    await adapter.initialize();
    await adapter.warmCardDatabase();

    // Game A: first VeryHard Priority request creates the pool + loads subset A.
    const actionA = await adapter.getAiAction("VeryHard", 0, "Priority");
    expect(actionA).not.toBeNull();
    const callsA = mockWorkerClient.loadCardDb.mock.calls;
    const loadedA = callsA[callsA.length - 1][0] as string;
    expect(loadedA).toContain("Game A Card");

    // Transition to game B: the pool subset is invalidated, instance preserved.
    await adapter.resetGameState();

    // Game B (disjoint deck): the pool rebuilds with game B's subset.
    const actionB = await adapter.getAiAction("VeryHard", 0, "Priority");
    expect(actionB).not.toBeNull();
    const callsB = mockWorkerClient.loadCardDb.mock.calls;
    const loadedB = callsB[callsB.length - 1][0] as string;
    // (c) game-B-exclusive card PRESENT; (b) game-A-exclusive card ABSENT.
    // Revert guard: dropping invalidateCardDb()/the ensureAiPool rebuild branch
    // leaves the pool loaded with subset A, so both assertions flip.
    expect(loadedB).toContain("Game B Card");
    expect(loadedB).not.toContain("Game A Card");
  });
});
