import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { WasmAdapter, getHostAdapter, getSharedAdapter } from "../wasm-adapter";
import { EngineWorkerClient } from "../engine-worker-client";
import type {
  AiActionProposal,
  AiDecisionDiagnosticReceipt,
  EngineAdapter,
  SubmitResult,
} from "../types";
import { AdapterError, AdapterErrorCode } from "../types";
import { buildGameState } from "../../test/factories/gameStateFactory";

const ensureWasmInit = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));
const resumeRestoredGameState = vi.hoisted(() => vi.fn());
const resumeMultiplayerHostState = vi.hoisted(() => vi.fn());

vi.mock("../../services/cardData", () => ({
  ensureWasmInit,
  ensureCardDatabase: vi.fn().mockResolvedValue(100),
}));

vi.mock("@wasm/engine", () => ({
  resume_restored_game_state: resumeRestoredGameState,
  resume_multiplayer_host_state: resumeMultiplayerHostState,
}));

// Mock EngineWorkerClient to avoid actual Worker creation in tests
const mockWorkerClient = {
  initialize: vi.fn().mockResolvedValue(undefined),
  loadCardDb: vi.fn().mockResolvedValue(100),
  loadCardDbFromUrl: vi.fn().mockResolvedValue(100),
  buildAiCardSubset: vi.fn(),
  evaluateDeckCompatibility: vi
    .fn()
    .mockResolvedValue({ standard: { compatible: true, reasons: [] } }),
  getCardFaceData: vi.fn().mockResolvedValue({ name: "Lightning Bolt" }),
  getCardParseDetails: vi.fn().mockResolvedValue([{ category: "ability" }]),
  getCardRulings: vi.fn().mockResolvedValue([{ date: "2020-01-01", text: "Test" }]),
  initializeGame: vi
    .fn()
    .mockResolvedValue({ events: [{ type: "GameStarted" }], log_entries: [] }),
  submitAction: vi
    .fn()
    .mockResolvedValue({ events: [], log_entries: [] } as SubmitResult),
  submitInteraction: vi.fn().mockResolvedValue({ events: [], log_entries: [] } as SubmitResult),
  previewManaPayment: vi.fn().mockResolvedValue([]),
  resolveAll: vi.fn().mockResolvedValue({ items_resolved: 0 }),
  getAiActionProposal: vi.fn(),
  getAiActionProposalWithDiagnostics: vi.fn(),
  getAiTacticalActionProposal: vi.fn(),
  getAiTacticalActionProposalWithDiagnostics: vi.fn(),
  getAiActionProposalFromScores: vi.fn(),
  getAiActionProposalFromScoresWithDiagnostics: vi.fn(),
  getAiScoredCandidates: vi.fn(),
  submitAiActionProposal: vi.fn(),
  getState: vi.fn().mockResolvedValue(buildGameState({
    turn_number: 1,
    phase: "Untap",
  })),
  getLegalActions: vi.fn().mockResolvedValue({ actions: [], autoPassRecommended: false }),
  exportState: vi.fn().mockResolvedValue("{}"),
  restoreState: vi.fn().mockResolvedValue(undefined),
  resumeRestoredGameState: vi.fn(),
  resumeMultiplayerHostState: vi.fn(),
  setMultiplayerMode: vi.fn().mockResolvedValue(undefined),
  resetGame: vi.fn().mockResolvedValue(undefined),
  applySeatMutation: vi.fn().mockResolvedValue({ state: {}, delta: {} }),
  ping: vi.fn().mockResolvedValue("phase-rs engine ready"),
  takeLastPanic: vi.fn().mockResolvedValue(null),
  dispose: vi.fn(),
};

vi.mock("../engine-worker-client", () => ({
  EngineWorkerClient: vi.fn().mockImplementation(function () {
    return mockWorkerClient;
  }),
}));

describe("WasmAdapter", () => {
  let adapter: WasmAdapter;

  afterEach(() => {
    vi.useRealTimers();
  });

  beforeEach(() => {
    vi.clearAllMocks();
    adapter = new WasmAdapter();
    mockWorkerClient.getState.mockResolvedValue(buildGameState({
      turn_number: 1,
      phase: "Untap",
    }));
    mockWorkerClient.buildAiCardSubset.mockResolvedValue(
      JSON.stringify({ kind: "subset", json: "{}", count: 0 }),
    );
    mockWorkerClient.getAiScoredCandidates.mockResolvedValue([]);
    mockWorkerClient.getAiActionProposal.mockResolvedValue(null);
    mockWorkerClient.getAiActionProposalWithDiagnostics.mockResolvedValue(null);
    mockWorkerClient.getAiTacticalActionProposal.mockResolvedValue(null);
    mockWorkerClient.getAiTacticalActionProposalWithDiagnostics.mockResolvedValue(null);
    mockWorkerClient.submitAiActionProposal.mockResolvedValue({
      status: "stale",
      reason: "test",
    });
    const restored = {
      presentation: {
        outcome: "noop" as const,
        automatedResolutionCount: 0,
        omittedEventCount: 0,
        logEntries: [],
      },
      snapshot: {
        state: buildGameState({ turn_number: 3, phase: "PreCombatMain" }),
        legalResult: { actions: [], autoPassRecommended: false },
      },
    };
    mockWorkerClient.resumeRestoredGameState.mockResolvedValue(restored);
    mockWorkerClient.resumeMultiplayerHostState.mockResolvedValue(restored);
  });

  describe("AI decision diagnostics", () => {
    const proposal: AiActionProposal = {
      token: "diagnostic-token",
      semanticOwner: 0,
      actor: 0,
      action: { type: "PassPriority" },
    };
    const receipt: AiDecisionDiagnosticReceipt = {
      semanticOwner: 0,
      authorizedActor: 0,
      selectedAction: { type: "PassPriority" },
      status: "direct",
      selectionExplanation: "A direct AI policy selected this action; no scored distribution was used.",
      samplingTemperature: null,
      candidates: [{
        action: { type: "PassPriority" },
        objectName: null,
        details: [],
        rank: null,
        isTopRanked: false,
        isSelected: true,
        score: null,
        weight: null,
        probability: null,
      }],
    };

    it("uses the legacy proposal endpoint while capture is disabled", async () => {
      mockWorkerClient.getAiActionProposal.mockResolvedValue(proposal);
      await adapter.initialize();

      await expect(adapter.getAiActionProposal("Medium", 0)).resolves.toEqual(proposal);

      expect(mockWorkerClient.getAiActionProposal).toHaveBeenCalledWith("Medium", 0);
      expect(mockWorkerClient.getAiActionProposalWithDiagnostics).not.toHaveBeenCalled();
    });

    it("publishes only after apply and retains a rejected proposal for retry", async () => {
      mockWorkerClient.getAiActionProposalWithDiagnostics.mockResolvedValue({ proposal, receipt });
      mockWorkerClient.submitAiActionProposal
        .mockResolvedValueOnce({
          status: "rejected",
          rejection: {
            code: "action_not_allowed",
            disposition: "unavailable",
            message: "That action is not allowed right now.",
            related_object_ids: [7],
          },
        })
        .mockResolvedValueOnce({ status: "applied", result: { events: [], log_entries: [] } });
      await adapter.initialize();
      const listener = vi.fn();
      adapter.setAiDecisionDiagnosticsEnabled(true);
      adapter.subscribeAiDecisionDiagnostics(listener);

      await expect(adapter.getAiActionProposal("Medium", 0)).resolves.toEqual(proposal);
      await expect(adapter.submitAiActionProposal(proposal)).resolves.toMatchObject({
        status: "rejected",
        rejection: { related_object_ids: [7] },
      });
      expect(listener).not.toHaveBeenCalled();

      await expect(adapter.submitAiActionProposal(proposal)).resolves.toMatchObject({ status: "applied" });
      expect(listener).toHaveBeenCalledOnce();
      expect(listener).toHaveBeenCalledWith(receipt);
    });

    it("suppresses stale proposal receipts", async () => {
      mockWorkerClient.getAiActionProposalWithDiagnostics.mockResolvedValue({ proposal, receipt });
      mockWorkerClient.submitAiActionProposal.mockResolvedValue({ status: "stale", reason: "old" });
      await adapter.initialize();
      const listener = vi.fn();
      adapter.setAiDecisionDiagnosticsEnabled(true);
      adapter.subscribeAiDecisionDiagnostics(listener);

      await adapter.getAiActionProposal("Medium", 0);
      await adapter.submitAiActionProposal(proposal);

      expect(listener).not.toHaveBeenCalled();
    });
  });

  it("retires a failed VeryHard pool before the next decision", async () => {
    const proposal: AiActionProposal = {
      token: "authoritative-token",
      semanticOwner: 0,
      actor: 0,
      action: { type: "PassPriority" },
    };
    mockWorkerClient.getState.mockResolvedValue({ waiting_for: { type: "Priority" } });
    mockWorkerClient.getAiScoredCandidates.mockRejectedValue(new Error("pool worker crashed"));
    mockWorkerClient.getAiActionProposal.mockResolvedValue(proposal);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      await adapter.initialize();
      adapter.cardDbLoaded = true;

      await expect(adapter.getAiActionProposal("VeryHard", 0)).resolves.toEqual(proposal);
      const firstPoolScoreCallCount = mockWorkerClient.getAiScoredCandidates.mock.calls.length;
      await expect(adapter.getAiActionProposal("VeryHard", 0)).resolves.toEqual(proposal);

      expect(firstPoolScoreCallCount).toBeGreaterThan(0);
      expect(mockWorkerClient.getAiScoredCandidates).toHaveBeenCalledTimes(firstPoolScoreCallCount);
      expect(mockWorkerClient.getAiActionProposal).toHaveBeenCalledTimes(2);
      expect(mockWorkerClient.getAiTacticalActionProposal).not.toHaveBeenCalled();
      expect(warn).toHaveBeenCalledOnce();
    } finally {
      warn.mockRestore();
    }
  });

  it("falls back after a stalled VeryHard pool score", async () => {
    vi.useFakeTimers();
    const proposal: AiActionProposal = {
      token: "authoritative-token",
      semanticOwner: 0,
      actor: 0,
      action: { type: "PassPriority" },
    };
    mockWorkerClient.getState.mockResolvedValue({ waiting_for: { type: "Priority" } });
    mockWorkerClient.getAiScoredCandidates.mockReturnValue(new Promise(() => {}));
    mockWorkerClient.getAiActionProposal.mockResolvedValue(proposal);
    mockWorkerClient.getAiTacticalActionProposal.mockResolvedValue(proposal);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      await adapter.initialize();
      adapter.cardDbLoaded = true;

      const decision = adapter.getAiActionProposal("VeryHard", 0);
      await vi.advanceTimersByTimeAsync(5_000);

      await expect(decision).resolves.toEqual(proposal);
      const firstPoolScoreCallCount = mockWorkerClient.getAiScoredCandidates.mock.calls.length;
      await expect(adapter.getAiActionProposal("VeryHard", 0)).resolves.toEqual(proposal);

      expect(mockWorkerClient.exportState).toHaveBeenCalledOnce();
      expect(firstPoolScoreCallCount).toBeGreaterThan(0);
      expect(mockWorkerClient.getAiScoredCandidates).toHaveBeenCalledTimes(firstPoolScoreCallCount);
      expect(mockWorkerClient.getAiTacticalActionProposal).toHaveBeenCalledOnce();
      expect(mockWorkerClient.getAiActionProposal).toHaveBeenCalledOnce();
      expect(warn).not.toHaveBeenCalled();
    } finally {
      warn.mockRestore();
    }
  });

  it("uses the tactical engine proposal when a diagnostic pool score times out", async () => {
    vi.useFakeTimers();
    const proposal: AiActionProposal = {
      token: "tactical-token",
      semanticOwner: 0,
      actor: 0,
      action: { type: "PassPriority" },
    };
    const receipt: AiDecisionDiagnosticReceipt = {
      semanticOwner: 0,
      authorizedActor: 0,
      selectedAction: { type: "PassPriority" },
      status: "direct",
      selectionExplanation: "The tactical fallback selected an engine-issued action.",
      samplingTemperature: null,
      candidates: [],
    };
    mockWorkerClient.getState.mockResolvedValue({ waiting_for: { type: "Priority" } });
    mockWorkerClient.getAiScoredCandidates.mockReturnValue(new Promise(() => {}));
    mockWorkerClient.getAiTacticalActionProposalWithDiagnostics.mockResolvedValue({ proposal, receipt });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      await adapter.initialize();
      adapter.cardDbLoaded = true;
      adapter.setAiDecisionDiagnosticsEnabled(true);

      const decision = adapter.getAiActionProposal("VeryHard", 0);
      await vi.advanceTimersByTimeAsync(5_000);

      await expect(decision).resolves.toEqual(proposal);
      expect(mockWorkerClient.getAiTacticalActionProposalWithDiagnostics).toHaveBeenCalledOnce();
      expect(mockWorkerClient.getAiActionProposalWithDiagnostics).not.toHaveBeenCalled();
      expect(warn).not.toHaveBeenCalled();
    } finally {
      warn.mockRestore();
    }
  });

  it("implements EngineAdapter interface", () => {
    const _check: EngineAdapter = adapter;
    expect(_check).toBeDefined();
    expect(typeof adapter.initialize).toBe("function");
    expect(typeof adapter.submitAction).toBe("function");
    expect(typeof adapter.getState).toBe("function");
    expect(typeof adapter.dispose).toBe("function");
  });

  describe("initialize", () => {
    it("creates worker client and initializes", async () => {
      await adapter.initialize();
      expect(mockWorkerClient.initialize).toHaveBeenCalledOnce();
    });

    it("is idempotent - second call is a no-op", async () => {
      await adapter.initialize();
      await adapter.initialize();
      expect(mockWorkerClient.initialize).toHaveBeenCalledOnce();
    });

    it("dedupes concurrent calls into one worker (no orphaned instance)", async () => {
      // Two callers race before the first settles (e.g. menu card-DB warm vs an
      // un-gated Resume click). Without the in-flight guard each would spawn a
      // worker, orphaning the first ~90 MB instance.
      await Promise.all([adapter.initialize(), adapter.initialize()]);
      expect(vi.mocked(EngineWorkerClient)).toHaveBeenCalledOnce();
      expect(mockWorkerClient.initialize).toHaveBeenCalledOnce();
    });

    it("disposes a worker that fails initialization and falls back to main-thread WASM", async () => {
      mockWorkerClient.initialize.mockRejectedValueOnce(
        new Error("WASM initialization failed"),
      );

      await expect(adapter.initialize()).resolves.toBeUndefined();

      expect(mockWorkerClient.dispose).toHaveBeenCalledOnce();
      expect(ensureWasmInit).toHaveBeenCalledOnce();
      expect(adapter.getEngineClient()).toBeNull();
    });

    it("rejects initialization canceled by disposal", async () => {
      let finishInitialization!: () => void;
      mockWorkerClient.initialize.mockImplementationOnce(
        () =>
          new Promise<void>((resolve) => {
            finishInitialization = resolve;
          }),
      );

      const staleInitialization = adapter.initialize();
      adapter.dispose();
      finishInitialization();
      await expect(staleInitialization).rejects.toMatchObject({
        code: AdapterErrorCode.NOT_INITIALIZED,
        message: "Adapter initialization was canceled. Please try again.",
      });

      await expect(adapter.ping()).rejects.toMatchObject({
        code: AdapterErrorCode.NOT_INITIALIZED,
      });

      await adapter.initialize();
      expect(vi.mocked(EngineWorkerClient)).toHaveBeenCalledTimes(2);
      await expect(adapter.ping()).resolves.toBe("phase-rs engine ready");
    });
  });

  describe("warmCardDatabase", () => {
    it("initializes and loads the card database, flipping the latch", async () => {
      await adapter.warmCardDatabase();
      expect(mockWorkerClient.initialize).toHaveBeenCalledOnce();
      expect(mockWorkerClient.loadCardDbFromUrl).toHaveBeenCalledOnce();
      expect(adapter.cardDbLoaded).toBe(true);
    });

    it("throws when the database fails to load (so the store can show error)", async () => {
      mockWorkerClient.loadCardDbFromUrl.mockRejectedValueOnce(new Error("boom"));
      await expect(adapter.warmCardDatabase()).rejects.toThrow();
      expect(adapter.cardDbLoaded).toBe(false);
    });
  });

  describe("checkDeckCompatibility", () => {
    it("ensures the DB is loaded then delegates to the worker", async () => {
      const request = { main_deck: ["Forest"], sideboard: [], commander: [] };
      const result = await adapter.checkDeckCompatibility(request);
      expect(mockWorkerClient.loadCardDbFromUrl).toHaveBeenCalledOnce();
      expect(mockWorkerClient.evaluateDeckCompatibility).toHaveBeenCalledWith(request);
      expect(result).toEqual({ standard: { compatible: true, reasons: [] } });
    });
  });

  describe("card display queries", () => {
    it("loads the DB once and routes every query through the shared worker", async () => {
      await expect(adapter.getCardFaceData("Lightning Bolt")).resolves.toEqual({
        name: "Lightning Bolt",
      });
      await expect(adapter.getCardParseDetails("Lightning Bolt")).resolves.toEqual([
        { category: "ability" },
      ]);
      await expect(adapter.getCardRulings("Lightning Bolt")).resolves.toEqual([
        { date: "2020-01-01", text: "Test" },
      ]);

      expect(mockWorkerClient.loadCardDbFromUrl).toHaveBeenCalledOnce();
      expect(mockWorkerClient.getCardFaceData).toHaveBeenCalledWith("Lightning Bolt");
      expect(mockWorkerClient.getCardParseDetails).toHaveBeenCalledWith("Lightning Bolt");
      expect(mockWorkerClient.getCardRulings).toHaveBeenCalledWith("Lightning Bolt");
    });
  });

  describe("submitAction", () => {
    const createCard = (count: number) => ({
      type: "Debug" as const,
      data: {
        type: "CreateCard" as const,
        data: {
          card_name: "Lightning Bolt",
          owner: 0,
          zone: "Hand" as const,
          run_etb: false,
          nonlegendary: false,
          count,
        },
      },
    });

    it("throws AdapterError with NOT_INITIALIZED if not initialized", async () => {
      await expect(
        adapter.submitAction({ type: "PassPriority" }, 0),
      ).rejects.toThrow(AdapterError);

      try {
        await adapter.submitAction({ type: "PassPriority" }, 0);
      } catch (error) {
        expect(error).toBeInstanceOf(AdapterError);
        const adapterError = error as AdapterError;
        expect(adapterError.code).toBe(AdapterErrorCode.NOT_INITIALIZED);
        expect(adapterError.recoverable).toBe(true);
      }
    });

    it("delegates to worker client", async () => {
      await adapter.initialize();
      await adapter.submitAction({ type: "PassPriority" }, 0);
      expect(mockWorkerClient.submitAction).toHaveBeenCalledWith(
        0,
        { type: "PassPriority" },
      );
    });

    it("submits a zero-count debug create without loading the card database", async () => {
      await adapter.initialize();

      await expect(adapter.submitAction(createCard(0), 0)).resolves.toEqual({
        events: [],
        log_entries: [],
      });

      expect(mockWorkerClient.submitAction).toHaveBeenCalledOnce();
      expect(mockWorkerClient.loadCardDbFromUrl).not.toHaveBeenCalled();
    });

    it("does not load the card database when Rust rejects debug-create preflight", async () => {
      mockWorkerClient.submitAction.mockRejectedValueOnce(
        new Error("Engine error: DebugAction is only allowed in Sandbox mode"),
      );
      await adapter.initialize();

      await expect(adapter.submitAction(createCard(1), 0)).rejects.toThrow(
        "DebugAction is only allowed in Sandbox mode",
      );

      expect(mockWorkerClient.submitAction).toHaveBeenCalledOnce();
      expect(mockWorkerClient.loadCardDbFromUrl).not.toHaveBeenCalled();
    });

    it("preserves a structured rejection without parsing its message", async () => {
      const rejection = {
        code: "wrong_player" as const,
        disposition: "unauthorized" as const,
        message: "That action belongs to a different player.",
        related_object_ids: [42],
      };
      mockWorkerClient.submitAction.mockRejectedValueOnce(
        new AdapterError(AdapterErrorCode.ACTION_REJECTED, rejection.message, true, undefined, rejection),
      );
      await adapter.initialize();

      await expect(adapter.submitAction({ type: "PassPriority" }, 0)).rejects.toMatchObject({
        code: AdapterErrorCode.ACTION_REJECTED,
        rejection,
      });
    });

    it("loads the card database and retries only after Rust admits a nonzero create", async () => {
      mockWorkerClient.submitAction
        .mockRejectedValueOnce(new Error("Engine error: card database not loaded"))
        .mockResolvedValueOnce({ events: [], log_entries: [] });
      await adapter.initialize();

      await expect(adapter.submitAction(createCard(1), 0)).resolves.toEqual({
        events: [],
        log_entries: [],
      });

      expect(mockWorkerClient.submitAction).toHaveBeenCalledTimes(2);
      expect(mockWorkerClient.loadCardDbFromUrl).toHaveBeenCalledOnce();
      expect(mockWorkerClient.submitAction.mock.invocationCallOrder[0])
        .toBeLessThan(mockWorkerClient.loadCardDbFromUrl.mock.invocationCallOrder[0]);
      expect(mockWorkerClient.loadCardDbFromUrl.mock.invocationCallOrder[0])
        .toBeLessThan(mockWorkerClient.submitAction.mock.invocationCallOrder[1]);
    });

    // Regression: state-loss classification splits on whether the panic
    // hook captured a message. ENGINE_PANIC must NOT be retried (re-running
    // the same input re-panics — the user-reported "ai-getAction-retry"
    // failure mode); STATE_LOST stays recoverable. Both pivots happen
    // inside `classifyEngineErrorAsync` and depend on `takeLastPanic`.
    describe("state-loss classification", () => {
      const stateLostError = new Error(
        "NOT_INITIALIZED: get_game_state returned null",
      );

      it("classifies as ENGINE_PANIC when panic was captured", async () => {
        await adapter.initialize();
        mockWorkerClient.submitAction.mockRejectedValueOnce(stateLostError);
        mockWorkerClient.takeLastPanic.mockResolvedValueOnce(
          "panicked at engine/src/foo.rs:42:1: assertion failed",
        );

        try {
          await adapter.submitAction({ type: "PassPriority" }, 0);
          expect.fail("expected ENGINE_PANIC");
        } catch (err) {
          expect(err).toBeInstanceOf(AdapterError);
          const adapterError = err as AdapterError;
          expect(adapterError.code).toBe(AdapterErrorCode.ENGINE_PANIC);
          expect(adapterError.recoverable).toBe(false);
          expect(adapterError.panic).toContain("assertion failed");
        }
      });

      it("classifies as STATE_LOST when no panic captured", async () => {
        await adapter.initialize();
        mockWorkerClient.submitAction.mockRejectedValueOnce(stateLostError);
        mockWorkerClient.takeLastPanic.mockResolvedValueOnce(null);

        try {
          await adapter.submitAction({ type: "PassPriority" }, 0);
          expect.fail("expected STATE_LOST");
        } catch (err) {
          expect(err).toBeInstanceOf(AdapterError);
          const adapterError = err as AdapterError;
          expect(adapterError.code).toBe(AdapterErrorCode.STATE_LOST);
          expect(adapterError.recoverable).toBe(true);
          expect(adapterError.panic).toBeUndefined();
        }
      });

      it("falls back to STATE_LOST when takeLastPanic itself rejects", async () => {
        // Defensive path — if the worker has truly died, the takePanic
        // request rejects (via onerror) and we must not propagate that
        // rejection. The user gets the legacy STATE_LOST flow rather than
        // a confusing secondary error.
        await adapter.initialize();
        mockWorkerClient.submitAction.mockRejectedValueOnce(stateLostError);
        mockWorkerClient.takeLastPanic.mockRejectedValueOnce(
          new Error("worker disposed"),
        );

        try {
          await adapter.submitAction({ type: "PassPriority" }, 0);
          expect.fail("expected STATE_LOST fallback");
        } catch (err) {
          expect(err).toBeInstanceOf(AdapterError);
          expect((err as AdapterError).code).toBe(AdapterErrorCode.STATE_LOST);
        }
      });
    });
  });

  describe("getState", () => {
    it("throws if not initialized", async () => {
      await expect(adapter.getState()).rejects.toThrow(AdapterError);
    });

    it("returns game state from worker", async () => {
      await adapter.initialize();
      const state = await adapter.getState();
      expect(state.turn_number).toBe(1);
      expect(state.active_player).toBe(0);
      expect(state.phase).toBe("Untap");
      expect(state.players).toHaveLength(2);
    });
  });

  describe("dispose", () => {
    it("cleans up state and prevents further operations", async () => {
      await adapter.initialize();
      adapter.dispose();
      expect(mockWorkerClient.dispose).toHaveBeenCalledOnce();
      await expect(adapter.getState()).rejects.toThrow(AdapterError);
    });
  });

  describe("restoreState", () => {
    it("serializes state to JSON and posts to worker", async () => {
      await adapter.initialize();

      const mockState = buildGameState({
        turn_number: 3,
        phase: "PreCombatMain",
        players: [],
      });

      await adapter.restoreState(mockState);
      expect(mockWorkerClient.loadCardDbFromUrl).toHaveBeenCalledOnce();
      expect(mockWorkerClient.restoreState).toHaveBeenCalledWith(
        JSON.stringify(mockState),
      );
      expect(mockWorkerClient.loadCardDbFromUrl.mock.invocationCallOrder[0])
        .toBeLessThan(mockWorkerClient.restoreState.mock.invocationCallOrder[0]);
    });

    it("throws if not initialized", async () => {
      const mockState = buildGameState();
      await expect(adapter.restoreState(mockState)).rejects.toThrow(AdapterError);
    });

    it("throws when the card database fails to load and does not restore", async () => {
      await adapter.initialize();
      mockWorkerClient.loadCardDbFromUrl.mockRejectedValueOnce(new Error("boom"));
      const mockState = buildGameState({
        turn_number: 3,
        phase: "PreCombatMain",
        players: [],
      });

      await expect(adapter.restoreState(mockState)).rejects.toThrow(
        "Card database failed to load",
      );
      expect(adapter.cardDbLoaded).toBe(false);
      expect(mockWorkerClient.restoreState).not.toHaveBeenCalled();
    });
  });

  describe("resumeMultiplayerHostState", () => {
    it("loads the card database then resumes on the worker", async () => {
      await adapter.initialize();
      const mockState = buildGameState({
        turn_number: 3,
        phase: "PreCombatMain",
        players: [],
      });

      await adapter.resumeMultiplayerHostState(mockState);
      expect(mockWorkerClient.loadCardDbFromUrl).toHaveBeenCalledOnce();
      expect(mockWorkerClient.resumeMultiplayerHostState).toHaveBeenCalledWith(
        JSON.stringify(mockState),
      );
      expect(mockWorkerClient.loadCardDbFromUrl.mock.invocationCallOrder[0])
        .toBeLessThan(
          mockWorkerClient.resumeMultiplayerHostState.mock.invocationCallOrder[0],
        );
    });

    it("throws when the card database fails to load and does not resume", async () => {
      await adapter.initialize();
      mockWorkerClient.loadCardDbFromUrl.mockRejectedValueOnce(new Error("boom"));
      const mockState = buildGameState({
        turn_number: 3,
        phase: "PreCombatMain",
        players: [],
      });

      await expect(adapter.resumeMultiplayerHostState(mockState)).rejects.toThrow(
        "Card database failed to load",
      );
      expect(adapter.cardDbLoaded).toBe(false);
      expect(mockWorkerClient.resumeMultiplayerHostState).not.toHaveBeenCalled();
    });

    it("propagates a queued main-thread fallback resume failure", async () => {
      mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
      resumeMultiplayerHostState.mockImplementationOnce(() => {
        throw new Error("resume failed");
      });
      await adapter.initialize();

      await expect(adapter.resumeMultiplayerHostState(buildGameState())).rejects.toThrow(
        "resume failed",
      );
      expect(resumeMultiplayerHostState).toHaveBeenCalledOnce();
    });
  });

  describe("resumeRestoredGameState", () => {
    it("returns the engine-authored presentation with its matching snapshot", async () => {
      await adapter.initialize();

      const resumed = await adapter.resumeRestoredGameState();

      expect(mockWorkerClient.resumeRestoredGameState).toHaveBeenCalledOnce();
      expect(resumed.presentation).toMatchObject({
        outcome: "noop",
        automatedResolutionCount: 0,
        omittedEventCount: 0,
      });
      expect(resumed.snapshot.state.turn_number).toBe(3);
    });
  });

  describe("applySeatMutation", () => {
    it("does not load the card database", async () => {
      await adapter.initialize();

      const mutation = JSON.stringify({ type: "AddAiSeat", difficulty: "Medium" });
      await adapter.applySeatMutation("{}", mutation);

      expect(mockWorkerClient.applySeatMutation).toHaveBeenCalledWith("{}", mutation);
      // Seat mutations are a pure reducer over the passed-in seat state plus the
      // static starter-deck table; the engine re-resolves against CARD_DB at
      // `initializeGame`. Warming it here would put a second full card database
      // in memory for every lobby seat change.
      expect(mockWorkerClient.loadCardDbFromUrl).not.toHaveBeenCalled();
      expect(adapter.cardDbLoaded).toBe(false);
    });
  });

  describe("initializeGame", () => {
    it("delegates to worker client with seed", async () => {
      await adapter.initialize();
      const result = await adapter.initializeGame();
      expect(result.events).toEqual([{ type: "GameStarted" }]);
      expect(mockWorkerClient.initializeGame).toHaveBeenCalledOnce();
    });

    it("loads card database when deck data is provided", async () => {
      await adapter.initialize();
      await adapter.initializeGame({ decks: [] });
      expect(mockWorkerClient.loadCardDbFromUrl).toHaveBeenCalledOnce();
    });
  });

  describe("getEngineClient", () => {
    it("returns null before initialization", () => {
      expect(adapter.getEngineClient()).toBeNull();
    });

    it("returns the worker client after initialization", async () => {
      await adapter.initialize();
      expect(adapter.getEngineClient()).toBe(mockWorkerClient);
    });
  });

});

/**
 * The device predicate is module-private and reads `navigator` on every call,
 * so both branches are driven here by redefining the properties it reads (same
 * technique as `PermanentCard.test.tsx`). Without this, the shared-engine path
 * would only ever run on the devices we cannot test.
 */
describe("getHostAdapter", () => {
  const realUserAgent = navigator.userAgent;

  function setMemoryConstrained(constrained: boolean): void {
    Object.defineProperty(navigator, "userAgent", {
      configurable: true,
      value: constrained
        ? "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15"
        : realUserAgent,
    });
    Object.defineProperty(navigator, "maxTouchPoints", { configurable: true, value: 0 });
  }

  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    setMemoryConstrained(false);
    // `dispose()` nulls the singleton, so a leaked shared adapter cannot
    // cross-talk into the next test.
    getSharedAdapter().dispose();
  });

  it("hands the host the tab's shared engine on a memory-constrained device", () => {
    setMemoryConstrained(true);
    expect(getHostAdapter()).toBe(getSharedAdapter());
  });

  it("gives the host its own engine everywhere else", () => {
    setMemoryConstrained(false);
    const host = getHostAdapter();
    expect(host).not.toBe(getSharedAdapter());
    host.dispose();
  });
});

describe("releaseHostSession", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    getSharedAdapter().dispose();
  });

  it("keeps the shared worker and its card database, clearing only what the host installed", async () => {
    const shared = getSharedAdapter();
    await shared.warmCardDatabase();
    expect(shared.cardDbLoaded).toBe(true);

    await shared.releaseHostSession(true);

    expect(getSharedAdapter()).toBe(shared);
    expect(shared.cardDbLoaded).toBe(true);
    expect(mockWorkerClient.dispose).not.toHaveBeenCalled();
    expect(mockWorkerClient.setMultiplayerMode).toHaveBeenCalledWith(false);
    expect(mockWorkerClient.resetGame).toHaveBeenCalledOnce();
    expect(mockWorkerClient.setMultiplayerMode.mock.invocationCallOrder[0])
      .toBeLessThan(mockWorkerClient.resetGame.mock.invocationCallOrder[0]);
  });

  it("leaves the shared engine completely untouched when the host never claimed it", async () => {
    const shared = getSharedAdapter();
    await shared.initialize();

    await shared.releaseHostSession(false);

    expect(mockWorkerClient.setMultiplayerMode).not.toHaveBeenCalled();
    expect(mockWorkerClient.resetGame).not.toHaveBeenCalled();
    expect(mockWorkerClient.dispose).not.toHaveBeenCalled();
  });

  it("disposes a private host engine outright, as teardown always did", async () => {
    const host = new WasmAdapter();
    await host.initialize();

    await host.releaseHostSession(true);

    expect(mockWorkerClient.dispose).toHaveBeenCalledOnce();
    await expect(host.getState()).rejects.toThrow(AdapterError);
    // A private release must never post the shared engine's flag clear.
    expect(mockWorkerClient.setMultiplayerMode).not.toHaveBeenCalled();
  });
});
