import { beforeEach, describe, expect, it, vi } from "vitest";

import { applyAiPoolCardDbPlan, resolveAiPoolCardDbPlan } from "../card-db-subset";
import { EngineWorkerClient } from "../engine-worker-client";
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

// ── VM-4: plan resolution + application ───────────────────────────────────
describe("resolveAiPoolCardDbPlan / applyAiPoolCardDbPlan", () => {
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

  it("resolves an unbounded universe (kind:full, e.g. Momir) without touching any pool", async () => {
    const { mainEngine } = makeMocks();
    mainEngine.buildAiCardSubset.mockResolvedValue(JSON.stringify({ kind: "full" }));

    const plan = await resolveAiPoolCardDbPlan("subset", mainEngine);

    // The caller must skip creating (or dispose) the pool instead of fanning
    // the full corpus (~380MB WASM linear memory per worker) across workers.
    expect(plan).toEqual({ kind: "unbounded" });
  });

  it("resolves and applies the subset for a bounded (non-Momir) game", async () => {
    const { mainEngine, aiPool } = makeMocks();
    const innerJson = '{"Bounded Card":{}}';
    mainEngine.buildAiCardSubset.mockResolvedValue(
      JSON.stringify({ kind: "subset", json: innerJson, count: 1 }),
    );

    const plan = await resolveAiPoolCardDbPlan("subset", mainEngine);
    expect(plan).toEqual({ kind: "subset", json: innerJson });
    if (plan.kind === "unbounded") throw new Error("unreachable");

    await applyAiPoolCardDbPlan(plan, aiPool);
    expect(aiPool.loadCardDbText).toHaveBeenCalledWith(innerJson);
    expect(aiPool.loadCardDb).not.toHaveBeenCalled();
  });

  it("mode=full resolves without consulting the engine and applies the full DB", async () => {
    const { mainEngine, aiPool } = makeMocks();

    const plan = await resolveAiPoolCardDbPlan("full", mainEngine);
    // Explicit user opt-in to the memory cost — the pool is kept.
    expect(plan).toEqual({ kind: "full" });
    expect(mainEngine.buildAiCardSubset).not.toHaveBeenCalled();
    if (plan.kind === "unbounded") throw new Error("unreachable");

    await applyAiPoolCardDbPlan(plan, aiPool);
    expect(aiPool.loadCardDb).toHaveBeenCalledOnce();
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

  it("falls through to single-worker getAiAction when selectActionFromScores returns null", async () => {
    const { WasmAdapter } = await import("../wasm-adapter");

    mockWorkerClient.buildAiCardSubset.mockResolvedValue(
      JSON.stringify({ kind: "subset", json: "{}", count: 0 }),
    );
    mockWorkerClient.getAiScoredCandidates.mockResolvedValue([
      [{ type: "PassPriority" }, 1.0],
      [{ type: "ActivateAbility", data: { source_id: 1, ability_index: 0 } }, 0.5],
    ]);
    mockWorkerClient.selectActionFromScores.mockResolvedValue(null);
    mockWorkerClient.getAiAction.mockResolvedValue({ type: "PassPriority" });

    const adapter = new WasmAdapter();
    await adapter.initialize();
    await adapter.warmCardDatabase();

    const action = await adapter.getAiAction("VeryHard", 0, "Priority");

    expect(mockWorkerClient.selectActionFromScores).toHaveBeenCalled();
    expect(mockWorkerClient.getAiAction).toHaveBeenCalledWith("VeryHard", 0);
    expect(action).toEqual({ type: "PassPriority" });
  });

  it("degrades to the single-worker path when the rebuild subset fails, then retries next decision", async () => {
    const { WasmAdapter } = await import("../wasm-adapter");

    mockWorkerClient.buildAiCardSubset
      .mockResolvedValueOnce(
        JSON.stringify({ kind: "subset", json: '{"Game A Card":{}}', count: 1 }),
      )
      .mockRejectedValueOnce(new Error("subset build failed"))
      .mockResolvedValueOnce(
        JSON.stringify({ kind: "subset", json: '{"Retry Card":{}}', count: 1 }),
      );
    mockWorkerClient.getAiAction.mockResolvedValue({ type: "PassPriority" });

    const adapter = new WasmAdapter();
    await adapter.initialize();
    await adapter.warmCardDatabase();

    // Game A: pool created and loaded normally.
    await adapter.getAiAction("VeryHard", 0, "Priority");

    // Game B: the rebuild's subset resolution rejects. The decision must NOT
    // reject (ensureAiPool is called outside getAiAction's try block) — it
    // degrades to the single-worker path for this decision.
    await adapter.resetGameState();
    const degraded = await adapter.getAiAction("VeryHard", 0, "Priority");
    expect(degraded).toEqual({ type: "PassPriority" });
    expect(mockWorkerClient.getAiAction).toHaveBeenCalled();

    // The failure is transient, not latched: the next decision retries the
    // subset build and restores the pool with the fresh subset.
    await adapter.getAiAction("VeryHard", 0, "Priority");
    expect(mockWorkerClient.buildAiCardSubset).toHaveBeenCalledTimes(3);
    const calls = mockWorkerClient.loadCardDb.mock.calls;
    expect(calls[calls.length - 1][0] as string).toContain("Retry Card");
  });

  it("drops the pool for an unbounded game (Momir) and restores it next game", async () => {
    const { WasmAdapter } = await import("../wasm-adapter");

    mockWorkerClient.buildAiCardSubset
      .mockResolvedValueOnce(JSON.stringify({ kind: "full" }))
      .mockResolvedValueOnce(
        JSON.stringify({ kind: "subset", json: '{"Bounded Card":{}}', count: 1 }),
      );
    mockWorkerClient.getAiAction.mockResolvedValue({ type: "PassPriority" });

    const adapter = new WasmAdapter();
    await adapter.initialize();
    await adapter.warmCardDatabase();
    const workersBefore = vi.mocked(EngineWorkerClient).mock.calls.length;

    // Momir game: the plan resolves to `unbounded` BEFORE any pool worker is
    // spawned — no new EngineWorkerClient instances are created, and the AI
    // answers via the single-worker path.
    const action = await adapter.getAiAction("VeryHard", 0, "Priority");
    expect(action).toEqual({ type: "PassPriority" });
    expect(vi.mocked(EngineWorkerClient).mock.calls.length).toBe(workersBefore);
    // Revert guard: without the unbounded branch the pool is created and the
    // scored-candidates path answers instead of engine.getAiAction.
    expect(mockWorkerClient.getAiAction).toHaveBeenCalled();

    // Second decision in the same game: the pool is NOT recreated just to
    // escalate again — the subset build ran exactly once.
    await adapter.getAiAction("VeryHard", 0, "Priority");
    expect(mockWorkerClient.buildAiCardSubset).toHaveBeenCalledTimes(1);

    // Next game is bounded: the pool comes back with that game's subset.
    await adapter.resetGameState();
    await adapter.getAiAction("VeryHard", 0, "Priority");
    expect(mockWorkerClient.buildAiCardSubset).toHaveBeenCalledTimes(2);
    const calls = mockWorkerClient.loadCardDb.mock.calls;
    expect(calls.length).toBeGreaterThan(0);
    expect(calls[calls.length - 1][0] as string).toContain("Bounded Card");
  });
});
