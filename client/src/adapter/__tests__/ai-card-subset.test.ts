import { beforeEach, describe, expect, it, vi } from "vitest";

import { applyAiPoolCardDbPlan, resolveAiPoolCardDbPlan } from "../card-db-subset";
import { EngineWorkerClient } from "../engine-worker-client";
import type { AiWorkerPool } from "../ai-worker-pool";
import type { AiActionProposal, GameState } from "../types";

const PASS_PROPOSAL: AiActionProposal = {
  token: "engine-issued",
  semanticOwner: 0,
  actor: 0,
  action: { type: "PassPriority" },
};

const mockWorkerClient = {
  initialize: vi.fn().mockResolvedValue(undefined),
  loadCardDb: vi.fn().mockResolvedValue(100),
  loadCardDbFromUrl: vi.fn().mockResolvedValue(100),
  buildAiCardSubset: vi.fn<() => Promise<string>>(),
  exportState: vi.fn().mockResolvedValue("{}"),
  restoreState: vi.fn().mockResolvedValue(undefined),
  getState: vi.fn<() => Promise<GameState>>().mockResolvedValue({
    waiting_for: { type: "Priority", data: { player: 0 } },
  } as GameState),
  getAiScoredCandidates: vi
    .fn()
    .mockResolvedValue([[{ type: "PassPriority" }, 1.0]]),
  getAiActionProposal: vi.fn().mockResolvedValue(PASS_PROPOSAL),
  getAiActionProposalFromScores: vi.fn().mockResolvedValue(PASS_PROPOSAL),
  resetGame: vi.fn().mockResolvedValue(undefined),
  takeLastPanic: vi.fn().mockResolvedValue(null),
  dispose: vi.fn(),
};

vi.mock("../engine-worker-client", () => ({
  EngineWorkerClient: vi.fn().mockImplementation(function () {
    return mockWorkerClient;
  }),
}));

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

  it("resolves an unbounded universe without creating a pool", async () => {
    const { mainEngine } = makeMocks();
    mainEngine.buildAiCardSubset.mockResolvedValue(JSON.stringify({ kind: "full" }));

    await expect(resolveAiPoolCardDbPlan("subset", mainEngine)).resolves.toEqual({
      kind: "unbounded",
    });
  });

  it("loads a bounded game subset rather than the full database", async () => {
    const { mainEngine, aiPool } = makeMocks();
    const json = '{"Bounded Card":{}}';
    mainEngine.buildAiCardSubset.mockResolvedValue(
      JSON.stringify({ kind: "subset", json, count: 1 }),
    );

    const plan = await resolveAiPoolCardDbPlan("subset", mainEngine);
    expect(plan).toEqual({ kind: "subset", json });
    if (plan.kind === "unbounded") throw new Error("expected bounded plan");
    await applyAiPoolCardDbPlan(plan, aiPool);
    expect(aiPool.loadCardDbText).toHaveBeenCalledWith(json);
    expect(aiPool.loadCardDb).not.toHaveBeenCalled();
  });

  it("uses the full database only for explicit full mode", async () => {
    const { mainEngine, aiPool } = makeMocks();
    const plan = await resolveAiPoolCardDbPlan("full", mainEngine);
    expect(plan).toEqual({ kind: "full" });
    expect(mainEngine.buildAiCardSubset).not.toHaveBeenCalled();
    if (plan.kind === "unbounded") throw new Error("expected full plan");
    await applyAiPoolCardDbPlan(plan, aiPool);
    expect(aiPool.loadCardDb).toHaveBeenCalledOnce();
  });
});

describe("WasmAdapter AI-pool subset lifecycle", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockWorkerClient.initialize.mockResolvedValue(undefined);
    mockWorkerClient.buildAiCardSubset.mockReset();
    mockWorkerClient.buildAiCardSubset.mockResolvedValue(
      JSON.stringify({ kind: "subset", json: "{}", count: 0 }),
    );
    mockWorkerClient.getState.mockResolvedValue({
      waiting_for: { type: "Priority", data: { player: 0 } },
    } as GameState);
    mockWorkerClient.getAiScoredCandidates.mockResolvedValue([
      [{ type: "PassPriority" }, 1.0],
    ]);
    mockWorkerClient.getAiActionProposal.mockResolvedValue(PASS_PROPOSAL);
    mockWorkerClient.getAiActionProposalFromScores.mockResolvedValue(PASS_PROPOSAL);
  });

  it("rebuilds the game-scoped subset after reset without leaking cards across games", async () => {
    const { WasmAdapter } = await import("../wasm-adapter");
    mockWorkerClient.buildAiCardSubset
      .mockResolvedValueOnce(JSON.stringify({
        kind: "subset", json: '{"Game A Card":{}}', count: 1,
      }))
      .mockResolvedValueOnce(JSON.stringify({
        kind: "subset", json: '{"Game B Card":{}}', count: 1,
      }));

    const adapter = new WasmAdapter();
    await adapter.initialize();
    await adapter.warmCardDatabase();
    await expect(adapter.getAiActionProposal("VeryHard", 0)).resolves.toEqual(PASS_PROPOSAL);
    const gameA = mockWorkerClient.loadCardDb.mock.calls[mockWorkerClient.loadCardDb.mock.calls.length - 1]?.[0] as string;
    expect(gameA).toContain("Game A Card");

    await adapter.resetGameState();
    await expect(adapter.getAiActionProposal("VeryHard", 0)).resolves.toEqual(PASS_PROPOSAL);
    const gameB = mockWorkerClient.loadCardDb.mock.calls[mockWorkerClient.loadCardDb.mock.calls.length - 1]?.[0] as string;
    expect(gameB).toContain("Game B Card");
    expect(gameB).not.toContain("Game A Card");
  });

  it("uses an authoritative single-worker proposal when scored candidates cannot be rebound", async () => {
    const { WasmAdapter } = await import("../wasm-adapter");
    mockWorkerClient.buildAiCardSubset.mockResolvedValue(
      JSON.stringify({ kind: "subset", json: "{}", count: 0 }),
    );
    mockWorkerClient.getAiActionProposalFromScores.mockResolvedValue(null);

    const adapter = new WasmAdapter();
    await adapter.initialize();
    await adapter.warmCardDatabase();

    await expect(adapter.getAiActionProposal("VeryHard", 0)).resolves.toEqual(PASS_PROPOSAL);
    expect(mockWorkerClient.getAiActionProposalFromScores).toHaveBeenCalled();
    expect(mockWorkerClient.getAiActionProposal).toHaveBeenCalledWith("VeryHard", 0);
  });

  it("disposes a partially initialized pool, does not reuse it, and retries after reset", async () => {
    const { WasmAdapter } = await import("../wasm-adapter");
    mockWorkerClient.buildAiCardSubset.mockResolvedValue(
      JSON.stringify({ kind: "subset", json: "{}", count: 0 }),
    );

    const adapter = new WasmAdapter();
    await adapter.initialize();
    await adapter.warmCardDatabase();
    const workersBeforePool = vi.mocked(EngineWorkerClient).mock.calls.length;
    mockWorkerClient.initialize.mockRejectedValueOnce(new Error("pool initialization timed out"));

    await expect(adapter.getAiActionProposal("VeryHard", 0)).resolves.toEqual(PASS_PROPOSAL);
    const workersAfterFailure = vi.mocked(EngineWorkerClient).mock.calls.length;
    const failedPoolSize = workersAfterFailure - workersBeforePool;
    expect(failedPoolSize).toBeGreaterThan(0);
    expect(mockWorkerClient.dispose).toHaveBeenCalledTimes(failedPoolSize);
    expect(mockWorkerClient.getAiScoredCandidates).not.toHaveBeenCalled();
    expect(mockWorkerClient.getAiActionProposal).toHaveBeenCalledTimes(1);

    await expect(adapter.getAiActionProposal("VeryHard", 0)).resolves.toEqual(PASS_PROPOSAL);
    expect(vi.mocked(EngineWorkerClient).mock.calls.length).toBe(workersAfterFailure);
    expect(mockWorkerClient.getAiActionProposal).toHaveBeenCalledTimes(2);

    await adapter.resetGameState();
    await adapter.getAiActionProposal("VeryHard", 0);
    expect(vi.mocked(EngineWorkerClient).mock.calls.length).toBeGreaterThan(workersAfterFailure);
    expect(mockWorkerClient.getAiScoredCandidates).toHaveBeenCalled();
  });

  it("shares one pool initialization between concurrent proposal requests", async () => {
    const { WasmAdapter } = await import("../wasm-adapter");
    const adapter = new WasmAdapter();
    await adapter.initialize();
    await adapter.warmCardDatabase();
    const workersBeforePool = vi.mocked(EngineWorkerClient).mock.calls.length;

    let finishPoolInitialization!: () => void;
    const poolInitialization = new Promise<void>((resolve) => {
      finishPoolInitialization = resolve;
    });
    mockWorkerClient.initialize.mockReturnValue(poolInitialization);

    const first = adapter.getAiActionProposal("VeryHard", 0);
    await vi.waitFor(() => {
      expect(vi.mocked(EngineWorkerClient).mock.calls.length).toBeGreaterThan(workersBeforePool);
    });
    const second = adapter.getAiActionProposal("VeryHard", 0);
    finishPoolInitialization();
    await Promise.all([first, second]);

    const expectedPoolSize = Math.max(2, Math.min((navigator.hardwareConcurrency ?? 0) - 1, 4));
    expect(vi.mocked(EngineWorkerClient).mock.calls.length - workersBeforePool).toBe(expectedPoolSize);
  });

  it.each([
    [
      "bounded",
      JSON.stringify({
        kind: "subset", json: '{"Stale Game Card":{}}', count: 1,
      }),
    ],
    ["unbounded", JSON.stringify({ kind: "full" })],
  ])("does not let a stale %s preserved-pool reload after a reset", async (_kind, stalePlanValue) => {
    const { WasmAdapter } = await import("../wasm-adapter");
    let resolveStalePlan!: (plan: string) => void;
    const stalePlan = new Promise<string>((resolve) => { resolveStalePlan = resolve; });
    let resolveCurrentPlan!: (plan: string) => void;
    const currentPlan = new Promise<string>((resolve) => { resolveCurrentPlan = resolve; });
    mockWorkerClient.buildAiCardSubset
      .mockResolvedValueOnce(JSON.stringify({
        kind: "subset", json: '{"Initial Game Card":{}}', count: 1,
      }))
      .mockReturnValueOnce(stalePlan)
      .mockReturnValueOnce(currentPlan);

    const adapter = new WasmAdapter();
    await adapter.initialize();
    await adapter.warmCardDatabase();
    await adapter.getAiActionProposal("VeryHard", 0);

    await adapter.resetGameState();
    const staleDecision = adapter.getAiActionProposal("VeryHard", 0);
    await vi.waitFor(() => expect(mockWorkerClient.buildAiCardSubset).toHaveBeenCalledTimes(2));
    const concurrentStaleDecision = adapter.getAiActionProposal("VeryHard", 0);
    await Promise.resolve();
    expect(mockWorkerClient.buildAiCardSubset).toHaveBeenCalledTimes(2);

    await adapter.resetGameState();
    const currentDecision = adapter.getAiActionProposal("VeryHard", 0);
    await vi.waitFor(() => expect(mockWorkerClient.buildAiCardSubset).toHaveBeenCalledTimes(3));
    resolveStalePlan(stalePlanValue);
    resolveCurrentPlan(JSON.stringify({ kind: "subset", json: '{"Current Game Card":{}}', count: 1 }));
    await Promise.all([staleDecision, concurrentStaleDecision, currentDecision]);

    const loaded = mockWorkerClient.loadCardDb.mock.calls.map(([text]) => text as string);
    expect(loaded.some((text) => text.includes("Stale Game Card"))).toBe(false);
    expect(loaded[loaded.length - 1]).toContain("Current Game Card");
  });

  it("drops the pool for an unbounded game and restores it for the next bounded game", async () => {
    const { WasmAdapter } = await import("../wasm-adapter");
    mockWorkerClient.buildAiCardSubset
      .mockResolvedValueOnce(JSON.stringify({ kind: "full" }))
      .mockResolvedValueOnce(JSON.stringify({
        kind: "subset", json: '{"Bounded Card":{}}', count: 1,
      }));

    const adapter = new WasmAdapter();
    await adapter.initialize();
    await adapter.warmCardDatabase();
    const workersBefore = vi.mocked(EngineWorkerClient).mock.calls.length;

    await expect(adapter.getAiActionProposal("VeryHard", 0)).resolves.toEqual(PASS_PROPOSAL);
    expect(vi.mocked(EngineWorkerClient).mock.calls.length).toBe(workersBefore);
    expect(mockWorkerClient.getAiActionProposal).toHaveBeenCalled();
    await adapter.getAiActionProposal("VeryHard", 0);
    expect(mockWorkerClient.buildAiCardSubset).toHaveBeenCalledTimes(1);

    await adapter.resetGameState();
    await adapter.getAiActionProposal("VeryHard", 0);
    expect(mockWorkerClient.buildAiCardSubset).toHaveBeenCalledTimes(2);
    const bounded = mockWorkerClient.loadCardDb.mock.calls[mockWorkerClient.loadCardDb.mock.calls.length - 1]?.[0] as string;
    expect(bounded).toContain("Bounded Card");
  });
});
