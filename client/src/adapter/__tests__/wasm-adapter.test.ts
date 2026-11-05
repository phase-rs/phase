import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  WasmAdapter,
  createHostSessionOwner,
  getHostAdapter,
  getSharedAdapter,
} from "../wasm-adapter";
import { EngineWorkerClient } from "../engine-worker-client";
import type {
  InteractionPreview,
  InteractionPreviewRequest,
} from "../generated/interaction";
import type {
  AiActionProposal,
  AiDecisionDiagnosticReceipt,
  EngineAdapter,
  GameEvent,
  SubmitResult,
} from "../types";
import { AdapterError, AdapterErrorCode } from "../types";
import {
  STATE_CODEC_DECODE_TIMEOUT_MS,
  StateCodecLifecycleError,
} from "../state-codec-runtime";
import { PLAYER_ID } from "../../constants/game";
import { buildGameState, gameStateFactory } from "../../test/factories/gameStateFactory";

const ensureWasmInit = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));
const resumeRestoredGameState = vi.hoisted(() => vi.fn());
const resumeMultiplayerHostState = vi.hoisted(() => vi.fn());
const previewInteractionJs = vi.hoisted(() => vi.fn());
const initializeMultiplayerHostGameJs = vi.hoisted(() =>
  vi.fn().mockReturnValue({ events: [], log_entries: [] }),
);
const getGameStateJs = vi.hoisted(() => vi.fn());
const getLegalActionsJs = vi.hoisted(() => vi.fn());
const getLegalActionsForViewerJs = vi.hoisted(() => vi.fn());
const getViewerTransitionSnapshotJs = vi.hoisted(() => vi.fn());
const setMultiplayerModeJs = vi.hoisted(() => vi.fn());
const clearGameStateJs = vi.hoisted(() => vi.fn());
const canonicalCardNamesJs = vi.hoisted(() => vi.fn());
const stateCodecDecodeJs = vi.hoisted(() => vi.fn());

vi.mock("../main-thread-engine", () => ({
  ensureWasmInit,
  ensureCardDatabase: vi.fn().mockResolvedValue(100),
  loadEngineModule: () => import("@wasm/engine"),
}));

vi.mock("@wasm/engine", () => ({
  resume_restored_game_state: resumeRestoredGameState,
  resume_multiplayer_host_state: resumeMultiplayerHostState,
  preview_interaction_js: previewInteractionJs,
  initialize_multiplayer_host_game: initializeMultiplayerHostGameJs,
  get_game_state: getGameStateJs,
  get_legal_actions_js: getLegalActionsJs,
  get_legal_actions_for_viewer_js: getLegalActionsForViewerJs,
  get_viewer_transition_snapshot_js: getViewerTransitionSnapshotJs,
  set_multiplayer_mode: setMultiplayerModeJs,
  clear_game_state: clearGameStateJs,
  canonicalCardNames: canonicalCardNamesJs,
  state_codec_decode: stateCodecDecodeJs,
}));

// Mock EngineWorkerClient to avoid actual Worker creation in tests
const mockWorkerClient = {
  initialize: vi.fn().mockResolvedValue(undefined),
  loadCardDb: vi.fn().mockResolvedValue(100),
  loadCardDbFromUrl: vi.fn().mockResolvedValue(100),
  buildAiCardSubset: vi.fn(),
  evaluateDeckCompatibility: vi
    .fn()
    .mockResolvedValue({
      standard: { compatible: true, reasons: [] },
      color_distribution: [],
    }),
  evaluateDeckFormatGate: vi.fn().mockResolvedValue({ compatible: true, reasons: [] }),
  customFormatFromLobbyConfig: vi.fn().mockResolvedValue({ label: "My Format" }),
  formatConfigForCustomRules: vi.fn().mockResolvedValue({ format: "Custom:0" }),
  getCardFaceData: vi.fn().mockResolvedValue({ name: "Lightning Bolt" }),
  getCardParseDetails: vi.fn().mockResolvedValue([{ category: "ability" }]),
  getCardRulings: vi.fn().mockResolvedValue([{ date: "2020-01-01", text: "Test" }]),
  canonicalCardNames: vi.fn().mockResolvedValue([]),
  openStateCodec: vi.fn().mockResolvedValue(1),
  decodeStateCodec: vi.fn().mockResolvedValue({ kind: "decoded", json: "{}" }),
  closeStateCodec: vi.fn().mockResolvedValue(undefined),
  query: vi.fn().mockResolvedValue("answer"),
  initializeGame: vi
    .fn()
    .mockResolvedValue({ events: [{ type: "GameStarted" }], log_entries: [] }),
  submitAction: vi
    .fn()
    .mockResolvedValue({ events: [], log_entries: [] } as SubmitResult),
  submitInteraction: vi.fn().mockResolvedValue({ events: [], log_entries: [] } as SubmitResult),
  previewManaPayment: vi.fn().mockResolvedValue([]),
  previewInteraction: vi.fn(),
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
  getSnapshot: vi.fn(),
  getViewerTransitionSnapshot: vi.fn(),
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
    mockWorkerClient.getAiActionProposalFromScores.mockResolvedValue(null);
    mockWorkerClient.getAiActionProposalFromScoresWithDiagnostics.mockResolvedValue(null);
    mockWorkerClient.getAiTacticalActionProposal.mockResolvedValue(null);
    mockWorkerClient.getAiTacticalActionProposalWithDiagnostics.mockResolvedValue(null);
    mockWorkerClient.submitAiActionProposal.mockResolvedValue({
      status: "stale",
      reason: "test",
    });
    getGameStateJs.mockReturnValue(buildGameState());
    getLegalActionsJs.mockReturnValue({ actions: [], autoPassRecommended: false });
    getLegalActionsForViewerJs.mockReturnValue({ actions: [], autoPassRecommended: false });
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

  it.each([false, true])("uses VeryHard pool scores from a state envelope with diagnostics %s", async (diagnostics) => {
    const proposal: AiActionProposal = {
      token: "scored-token",
      semanticOwner: 0,
      actor: 0,
      action: { type: "PassPriority" },
    };
    const receipt: AiDecisionDiagnosticReceipt = {
      semanticOwner: 0,
      authorizedActor: 0,
      selectedAction: proposal.action,
      status: "direct",
      selectionExplanation: "The authoritative worker selected the scored action.",
      samplingTemperature: null,
      candidates: [],
    };
    const scores = [[proposal.action, 12]];
    mockWorkerClient.getState.mockResolvedValue({
      state: gameStateFactory.priority().build(),
      derived: {},
    });
    mockWorkerClient.getAiScoredCandidates.mockResolvedValue(scores);
    mockWorkerClient.getAiActionProposalFromScores.mockResolvedValue(proposal);
    mockWorkerClient.getAiActionProposalFromScoresWithDiagnostics.mockResolvedValue({ proposal, receipt });
    mockWorkerClient.submitAiActionProposal.mockResolvedValue({
      status: "applied",
      result: { events: [], log_entries: [] },
    });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      await adapter.initialize();
      adapter.cardDbLoaded = true;
      adapter.setAiDecisionDiagnosticsEnabled(diagnostics);
      const listener = vi.fn();
      adapter.subscribeAiDecisionDiagnostics(listener);

      await expect(adapter.getAiActionProposal("VeryHard", 0)).resolves.toEqual(proposal);

      expect(mockWorkerClient.getAiScoredCandidates).toHaveBeenCalledWith("VeryHard", 0, expect.any(Number));
      const selectedEndpoint = diagnostics
        ? mockWorkerClient.getAiActionProposalFromScoresWithDiagnostics
        : mockWorkerClient.getAiActionProposalFromScores;
      const unusedEndpoint = diagnostics
        ? mockWorkerClient.getAiActionProposalFromScores
        : mockWorkerClient.getAiActionProposalFromScoresWithDiagnostics;
      expect(selectedEndpoint).toHaveBeenCalledExactlyOnceWith(JSON.stringify(scores), "VeryHard", 0, expect.any(Number));
      expect(unusedEndpoint).not.toHaveBeenCalled();
      expect(mockWorkerClient.getAiActionProposal).not.toHaveBeenCalled();
      expect(mockWorkerClient.getAiActionProposalWithDiagnostics).not.toHaveBeenCalled();
      expect(mockWorkerClient.getAiTacticalActionProposal).not.toHaveBeenCalled();
      expect(mockWorkerClient.getAiTacticalActionProposalWithDiagnostics).not.toHaveBeenCalled();
      expect(warn).not.toHaveBeenCalled();
      expect(listener).not.toHaveBeenCalled();

      await expect(adapter.submitAiActionProposal(proposal)).resolves.toMatchObject({ status: "applied" });
      expect(mockWorkerClient.submitAiActionProposal).toHaveBeenCalledExactlyOnceWith(proposal);
      if (diagnostics) {
        expect(listener).toHaveBeenCalledExactlyOnceWith(receipt);
      } else {
        expect(listener).not.toHaveBeenCalled();
      }
    } finally {
      warn.mockRestore();
    }
  });

  it.each([false, true])("skips VeryHard pool scoring for a non-Priority envelope with diagnostics %s", async (diagnostics) => {
    mockWorkerClient.getState.mockResolvedValue({
      state: gameStateFactory.manaPayment().build(),
      derived: {},
    });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      await adapter.initialize();
      adapter.cardDbLoaded = true;
      adapter.setAiDecisionDiagnosticsEnabled(diagnostics);

      await expect(adapter.getAiActionProposal("VeryHard", 0)).resolves.toBeNull();

      const selectedEndpoint = diagnostics
        ? mockWorkerClient.getAiActionProposalWithDiagnostics
        : mockWorkerClient.getAiActionProposal;
      expect(selectedEndpoint).toHaveBeenCalledExactlyOnceWith("VeryHard", 0);
      expect(mockWorkerClient.exportState).not.toHaveBeenCalled();
      expect(mockWorkerClient.getAiScoredCandidates).not.toHaveBeenCalled();
      expect(mockWorkerClient.getAiActionProposalFromScores).not.toHaveBeenCalled();
      expect(mockWorkerClient.getAiActionProposalFromScoresWithDiagnostics).not.toHaveBeenCalled();
      expect(mockWorkerClient.getAiTacticalActionProposal).not.toHaveBeenCalled();
      expect(mockWorkerClient.getAiTacticalActionProposalWithDiagnostics).not.toHaveBeenCalled();
      expect(warn).not.toHaveBeenCalled();
    } finally {
      warn.mockRestore();
    }
  });

  it("retires a failed VeryHard pool before the next decision", async () => {
    const proposal: AiActionProposal = {
      token: "authoritative-token",
      semanticOwner: 0,
      actor: 0,
      action: { type: "PassPriority" },
    };
    mockWorkerClient.getState.mockResolvedValue({
      state: gameStateFactory.priority().build(),
      derived: {},
    });
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
    mockWorkerClient.getState.mockResolvedValue({
      state: gameStateFactory.priority().build(),
      derived: {},
    });
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
    mockWorkerClient.getState.mockResolvedValue({
      state: gameStateFactory.priority().build(),
      derived: {},
    });
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
      expect(result).toEqual({
        standard: { compatible: true, reasons: [] },
        color_distribution: [],
      });
    });
  });

  // The pool that actually broke the live site was fetched fine and then
  // rejected by serde, so these pin the distinction the old code lost: a
  // schema-rejected database must not be reported as an uncalled loader.
  describe("card database load failure reaches the caller", () => {
    const SCHEMA_ERROR =
      "Failed to parse card database: unknown variant `Tap`, expected one of `DealDamage`, `SetTapState`";

    it("reports the underlying cause, not a missing-loader message", async () => {
      mockWorkerClient.loadCardDbFromUrl.mockRejectedValueOnce(new Error(SCHEMA_ERROR));
      const err = await adapter
        .checkDeckCompatibility({ main_deck: ["Forest"] })
        .then(() => null, (e: Error) => e);
      expect(err).toBeInstanceOf(Error);
      expect(err!.message).toContain("unknown variant `Tap`");
      expect(err!.message).not.toContain("Call loadCardDb");
    });

    it("does not consult the worker once the database is known to be absent", async () => {
      mockWorkerClient.loadCardDbFromUrl.mockRejectedValueOnce(new Error(SCHEMA_ERROR));
      await expect(
        adapter.checkDeckCompatibility({ main_deck: ["Forest"] }),
      ).rejects.toThrow();
      expect(mockWorkerClient.evaluateDeckCompatibility).not.toHaveBeenCalled();
    });

    // The discriminator: without this, every assertion above would still pass
    // if the strict gate simply rejected unconditionally.
    it("still delegates normally when the database loads", async () => {
      const request = { main_deck: ["Forest"] };
      await expect(adapter.checkDeckCompatibility(request)).resolves.toEqual({
        standard: { compatible: true, reasons: [] },
        color_distribution: [],
      });
      expect(mockWorkerClient.evaluateDeckCompatibility).toHaveBeenCalledWith(request);
    });

    // serde names every variant it rejected, which is thousands of characters.
    it("trims a very long cause but keeps the diagnostic head", async () => {
      const longCause = `${SCHEMA_ERROR}${", `Filler`".repeat(400)}`;
      mockWorkerClient.loadCardDbFromUrl.mockRejectedValueOnce(new Error(longCause));
      const err = await adapter
        .checkDeckCompatibility({ main_deck: ["Forest"] })
        .then(() => null, (e: Error) => e);
      expect(err).toBeInstanceOf(Error);
      expect(err!.message).toContain("unknown variant `Tap`");
      expect(err!.message).toMatch(/…$/);
      expect(err!.message.length).toBeLessThan(longCause.length);
      // The full text stays reachable for diagnosis even though the message is trimmed.
      expect((err as Error & { cause?: Error }).cause?.message).toBe(longCause);
    });

    it("applies to game creation too, not only the compatibility chip", async () => {
      mockWorkerClient.loadCardDbFromUrl.mockRejectedValueOnce(new Error(SCHEMA_ERROR));
      await adapter.initialize();
      await expect(
        adapter.initializeGame({ main_deck: ["Forest"] }),
      ).rejects.toThrow("unknown variant `Tap`");
      expect(mockWorkerClient.initializeGame).not.toHaveBeenCalled();
    });
  });

  // Symmetric with the block above, and load-bearing for exactly one reason: a
  // copy-paste slip inside `evaluateDeckFormatGate`'s real implementation —
  // calling `evaluateDeckCompatibility` instead of `evaluateDeckFormatGate` —
  // would silently restore the UI-hint path's "no opinion" answer on the
  // security gate, and every fully-mocked `p2p-adapter` test would still pass.
  // This is the only layer that can catch it.
  describe("evaluateDeckFormatGate", () => {
    it("ensures the DB is loaded then delegates to the gate worker method", async () => {
      const request = { main_deck: ["Forest"], sideboard: [], selected_format: "Custom:0" };
      const result = await adapter.evaluateDeckFormatGate(request);
      expect(mockWorkerClient.loadCardDbFromUrl).toHaveBeenCalledOnce();
      expect(mockWorkerClient.evaluateDeckFormatGate).toHaveBeenCalledWith(request);
      // The UI-hint method must NOT be what a gate call reaches.
      expect(mockWorkerClient.evaluateDeckCompatibility).not.toHaveBeenCalled();
      expect(result).toEqual({ compatible: true, reasons: [] });
    });
  });

  describe("custom format save/select", () => {
    it("delegates a lobby-config save to the engine", async () => {
      const config = { format: "Commander" };
      const result = await adapter.customFormatFromLobbyConfig("My Format", config);
      expect(mockWorkerClient.customFormatFromLobbyConfig).toHaveBeenCalledWith(
        "My Format",
        config,
      );
      expect(result).toEqual({ label: "My Format" });
    });

    it("delegates custom-rule resolution to the engine's own resolver", async () => {
      const rules = { id: 0 };
      const result = await adapter.formatConfigForCustomRules(rules);
      expect(mockWorkerClient.formatConfigForCustomRules).toHaveBeenCalledWith(rules);
      expect(result).toEqual({ format: "Custom:0" });
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

    it("canonicalCardNames ensures the DB is loaded then delegates to the worker", async () => {
      mockWorkerClient.canonicalCardNames.mockResolvedValueOnce(["Revival // Revenge", null]);

      await expect(
        adapter.canonicalCardNames(["Revival/Revenge", "Not A Card"]),
      ).resolves.toEqual(["Revival // Revenge", null]);

      expect(mockWorkerClient.loadCardDbFromUrl).toHaveBeenCalledOnce();
      expect(mockWorkerClient.canonicalCardNames).toHaveBeenCalledWith([
        "Revival/Revenge",
        "Not A Card",
      ]);
    });
  });

  describe("query", () => {
    it("loads the card database before a query that reads it", async () => {
      await expect(adapter.query("classifyDeck", ["Forest"])).resolves.toBe("answer");

      expect(mockWorkerClient.loadCardDbFromUrl).toHaveBeenCalledOnce();
      expect(mockWorkerClient.query).toHaveBeenCalledWith("classifyDeck", [["Forest"]]);
    });

    // The Play setup screen asks for the format's deck supply before any game
    // starts; it must not pull the ~90 MB database in to answer.
    it("answers a format query without loading the card database", async () => {
      await expect(adapter.query("deckSupplyForFormat", "Standard")).resolves.toBe("answer");

      expect(mockWorkerClient.loadCardDbFromUrl).not.toHaveBeenCalled();
      expect(mockWorkerClient.query).toHaveBeenCalledWith("deckSupplyForFormat", ["Standard"]);
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
          creation_kind: "Card" as const,
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
        PLAYER_ID,
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

  describe("local-seat legal actions", () => {
    const seatList = { actions: [], autoPassRecommended: false, seat: PLAYER_ID };

    it("asks the worker for the local seat's own list on every read", async () => {
      await adapter.initialize();
      mockWorkerClient.getSnapshot.mockResolvedValue({
        state: buildGameState(),
        legalResult: seatList,
      });

      await adapter.getLegalActions();
      await adapter.getSnapshot();
      await adapter.resumeRestoredGameState();
      await adapter.resumeMultiplayerHostState(buildGameState());

      expect(mockWorkerClient.getLegalActions).toHaveBeenCalledWith(PLAYER_ID);
      expect(mockWorkerClient.getSnapshot).toHaveBeenCalledWith(PLAYER_ID);
      expect(mockWorkerClient.resumeRestoredGameState).toHaveBeenCalledWith(PLAYER_ID);
      expect(mockWorkerClient.resumeMultiplayerHostState).toHaveBeenCalledWith(
        expect.any(String),
        PLAYER_ID,
      );
    });

    it("main-thread fallback reads the viewer-scoped export, never the seat-agnostic one", async () => {
      mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
      getLegalActionsForViewerJs.mockReturnValue(seatList);
      resumeRestoredGameState.mockReturnValue({
        outcome: "noop",
        automatedResolutionCount: 0,
        omittedEventCount: 0,
        logEntries: [],
      });
      resumeMultiplayerHostState.mockReturnValue({
        outcome: "noop",
        automatedResolutionCount: 0,
        omittedEventCount: 0,
        logEntries: [],
      });
      await adapter.initialize();

      const lists = [
        await adapter.getLegalActions(),
        (await adapter.getSnapshot()).legalResult,
        (await adapter.resumeRestoredGameState()).snapshot.legalResult,
        (await adapter.resumeMultiplayerHostState(buildGameState())).snapshot.legalResult,
      ];

      expect(lists).toEqual(Array(4).fill(seatList));
      expect(getLegalActionsForViewerJs).toHaveBeenCalledTimes(4);
      expect(getLegalActionsForViewerJs).toHaveBeenCalledWith(PLAYER_ID);
      expect(getLegalActionsJs).not.toHaveBeenCalled();
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

  it("clears the claimed shared main-thread fallback through its runtime queue", async () => {
    const shared = getSharedAdapter();
    const owner = createHostSessionOwner();
    mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
    await shared.initialize();
    await shared.initializeMultiplayerHostGame(undefined, undefined, undefined, undefined, undefined, owner);

    await shared.releaseHostSession(true, owner);

    expect(setMultiplayerModeJs).toHaveBeenCalledWith(false);
    expect(clearGameStateJs).toHaveBeenCalledOnce();
    expect(getSharedAdapter()).toBe(shared);
  });

  it("records ownership when restoring a main-thread fallback host", async () => {
    const host = new WasmAdapter();
    const owner = createHostSessionOwner();
    mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
    resumeMultiplayerHostState.mockReturnValueOnce({
      presentation: {
        outcome: "noop",
        automatedResolutionCount: 0,
        omittedEventCount: 0,
        logEntries: [],
      },
      snapshot: {
        state: buildGameState(),
        legalResult: { actions: [], autoPassRecommended: false },
      },
    });
    await host.initialize();
    await host.resumeMultiplayerHostState(buildGameState(), owner);

    await host.releaseHostSession(true, owner);

    expect(setMultiplayerModeJs).toHaveBeenCalledWith(false);
    expect(clearGameStateJs).toHaveBeenCalledOnce();
  });

  it("leaves an unclaimed main-thread fallback untouched", async () => {
    const shared = getSharedAdapter();
    mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
    await shared.initialize();

    await shared.releaseHostSession(false);

    expect(setMultiplayerModeJs).not.toHaveBeenCalled();
    expect(clearGameStateJs).not.toHaveBeenCalled();
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

  it("clears a claimed private main-thread fallback before disposing it", async () => {
    const host = new WasmAdapter();
    const owner = createHostSessionOwner();
    mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
    await host.initialize();
    await host.initializeMultiplayerHostGame(undefined, undefined, undefined, undefined, undefined, owner);

    await host.releaseHostSession(true, owner);

    expect(setMultiplayerModeJs).toHaveBeenCalledWith(false);
    expect(clearGameStateJs).toHaveBeenCalledOnce();
    expect(mockWorkerClient.dispose).toHaveBeenCalledOnce();
    await expect(host.getState()).rejects.toThrow(AdapterError);
  });

  it("does not let a stale fallback release clear a newer host generation", async () => {
    const shared = getSharedAdapter();
    const firstOwner = createHostSessionOwner();
    const secondOwner = createHostSessionOwner();
    mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
    await shared.initialize();
    await shared.initializeMultiplayerHostGame(undefined, undefined, undefined, undefined, undefined, firstOwner);
    await shared.initializeMultiplayerHostGame(undefined, undefined, undefined, undefined, undefined, secondOwner);

    await shared.releaseHostSession(true, firstOwner);
    await shared.resetGameState();

    expect(setMultiplayerModeJs).not.toHaveBeenCalled();
    expect(clearGameStateJs).not.toHaveBeenCalled();

    await shared.releaseHostSession(true, secondOwner);

    expect(setMultiplayerModeJs).toHaveBeenCalledWith(false);
    expect(clearGameStateJs).toHaveBeenCalledOnce();
  });
});

const request = {
  requestId: "req-1" as InteractionPreviewRequest["requestId"],
  interactionId: "int-1" as InteractionPreviewRequest["interactionId"],
  response: { type: "shortcut", data: { decision: { type: "acceptSuggested" }, pins: [] } },
} as InteractionPreviewRequest;

const answer = {
  requestId: request.requestId,
  interactionId: request.interactionId,
  status: { type: "confirmable" },
  progress: { selected: 1, minimum: 1, maximum: 1, aggregate: null, confirmable: true },
  outcome: "advanced",
  summaries: [],
  shortcutPreview: {
    count: 4,
    entries: [{ family: "life", player: 2, amount: -8 }],
    allocation: [{ choiceId: "int-1.k0", amount: 3 }, { choiceId: "int-1.k1", amount: 1 }],
  },
} as unknown as InteractionPreview;

const viewerTransitionEvents: GameEvent[] = [{ type: "GameStarted" }];

describe("WasmAdapter.getViewerTransitionSnapshot", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("forwards the engine-owned snapshot through the worker boundary", async () => {
    const rawState = buildGameState({ turn_number: 4, phase: "PreCombatMain" });
    const snapshot = {
      state: { state: rawState, derived: {} },
      actions: [],
      autoPassRecommended: false,
      events: viewerTransitionEvents,
    };
    mockWorkerClient.getViewerTransitionSnapshot.mockResolvedValue(snapshot);
    const adapter = new WasmAdapter();
    await adapter.initialize();

    const result = await adapter.getViewerTransitionSnapshot(1, viewerTransitionEvents);

    expect(mockWorkerClient.getViewerTransitionSnapshot).toHaveBeenCalledExactlyOnceWith(
      1,
      viewerTransitionEvents,
    );
    expect(result.events).toEqual(viewerTransitionEvents);
    expect(result.state).toMatchObject(rawState);
    expect(result.state.derived).toEqual({});
  });

  it("uses the same typed boundary on the main-thread fallback", async () => {
    const rawState = buildGameState({ turn_number: 5, phase: "PostCombatMain" });
    getViewerTransitionSnapshotJs.mockReturnValue({
      state: { state: rawState, derived: {} },
      actions: [],
      autoPassRecommended: false,
      events: viewerTransitionEvents,
    });
    mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
    const adapter = new WasmAdapter();
    await adapter.initialize();

    const result = await adapter.getViewerTransitionSnapshot(1, viewerTransitionEvents);

    expect(getViewerTransitionSnapshotJs).toHaveBeenCalledExactlyOnceWith(
      1,
      viewerTransitionEvents,
    );
    expect(mockWorkerClient.getViewerTransitionSnapshot).not.toHaveBeenCalled();
    expect(result.events).toEqual(viewerTransitionEvents);
    expect(result.state).toMatchObject(rawState);
  });
});

describe("WasmAdapter.canonicalCardNames", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("canonicalCardNames uses the main-thread fallback", async () => {
    canonicalCardNamesJs.mockReturnValue(["Revival // Revenge"]);
    mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
    const adapter = new WasmAdapter();
    await adapter.initialize();

    const result = await adapter.canonicalCardNames(["Revival/Revenge"]);

    expect(canonicalCardNamesJs).toHaveBeenCalledExactlyOnceWith(["Revival/Revenge"]);
    expect(mockWorkerClient.canonicalCardNames).not.toHaveBeenCalled();
    expect(result).toEqual(["Revival // Revenge"]);
  });
});

describe("WasmAdapter.previewInteraction", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockWorkerClient.previewInteraction.mockResolvedValue(answer);
  });

  it("forwards the whole engine answer through the worker client", async () => {
    const adapter = new WasmAdapter();
    await adapter.initialize();

    const preview = await adapter.previewInteraction!(request, 1);

    expect(mockWorkerClient.previewInteraction).toHaveBeenCalledExactlyOnceWith(1, request);
    expect(preview).toEqual(answer);
    expect(preview.shortcutPreview?.entries).toEqual(answer.shortcutPreview?.entries);
    expect(preview.requestId).toBe(request.requestId);
  });

  it("round-trips an answer that carries no payload", async () => {
    const { shortcutPreview: _dropped, ...withoutPayload } = answer;
    mockWorkerClient.previewInteraction.mockResolvedValue(withoutPayload as InteractionPreview);
    const adapter = new WasmAdapter();
    await adapter.initialize();

    const preview = await adapter.previewInteraction!(request, 1);

    // Paired positive for the leg above: the adapter is forwarding the ANSWER, so it cannot be
    // passing the payload leg by having dropped everything else.
    expect(preview.shortcutPreview).toBeUndefined();
    expect(preview.requestId).toBe(request.requestId);
    expect(preview.status).toEqual({ type: "confirmable" });
  });

  it("carries the capability on the main-thread fallback too", async () => {
    previewInteractionJs.mockReturnValue({ status: "applied", result: answer });
    mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
    const adapter = new WasmAdapter();
    await adapter.initialize();

    const preview = await adapter.previewInteraction!(request, 1);

    expect(previewInteractionJs).toHaveBeenCalledExactlyOnceWith(1, request);
    expect(mockWorkerClient.previewInteraction).not.toHaveBeenCalled();
    expect(preview).toEqual(answer);
  });

  it("surfaces a rejected fallback envelope as an AdapterError", async () => {
    previewInteractionJs.mockReturnValue({
      status: "rejected",
      rejection: {
        code: "invalid_interaction_response",
        disposition: "invalid",
        message: "That response is not valid.",
        related_object_ids: [],
      },
    });
    mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
    const adapter = new WasmAdapter();
    await adapter.initialize();

    await expect(adapter.previewInteraction!(request, 1)).rejects.toBeInstanceOf(AdapterError);
  });
});

// ── The worker envelope's `type` literal, both ends. ─────────────────────────────────────────
//
// `request()` takes a `Record<string, unknown>` and the dispatch switch has no `default:`, so
// nothing in the compiler relates the string one end posts to the one the other end handles.
// This row reads both modules as TEXT and compares them; it proves `type`-literal agreement and
// never that either body runs.

/** Every `type` literal `EngineWorkerClient` posts through `request()`, typed or untyped. */
function postedMessageTypes(source: string): string[] {
  return Array.from(
    source.matchAll(/this\.request\b[^(]*\(\s*\{\s*type:\s*"([A-Za-z0-9_]+)"/g),
    (m) => m[1],
  );
}

/** Call sites, so a call shape the extractor cannot read reds the row instead of shrinking it. */
function requestCallSites(source: string): number {
  return Array.from(source.matchAll(/this\.request\b/g)).length;
}

/** The body of the dispatch `switch (msg.type)`, brace-matched. */
function dispatchSwitchBody(source: string): string | null {
  const at = source.indexOf("switch (msg.type)");
  if (at < 0) return null;
  const open = source.indexOf("{", at);
  let depth = 0;
  for (let i = open; i < source.length; i++) {
    if (source[i] === "{") depth++;
    else if (source[i] === "}" && --depth === 0) return source.slice(open + 1, i);
  }
  return null;
}

/** `case` labels at the dispatch switch's own brace depth. */
function handledMessageTypes(source: string): string[] {
  const body = dispatchSwitchBody(source);
  if (body === null) return [];
  const out: string[] = [];
  let depth = 0;
  for (const line of body.split("\n")) {
    if (depth === 0) {
      const m = /^\s*case "([A-Za-z0-9_]+)":/.exec(line);
      if (m) out.push(m[1]);
    }
    for (const c of line) depth += c === "{" ? 1 : c === "}" ? -1 : 0;
  }
  return out;
}

function lockstepVerdict(clientSource: string, workerSource: string) {
  const posted = postedMessageTypes(clientSource);
  const handled = new Set(handledMessageTypes(workerSource));
  return {
    walkedAll: posted.length === requestCallSites(clientSource),
    reach: ["previewInteraction", "submitInteraction"].filter((type) => posted.includes(type)),
    missing: [...new Set(posted.filter((type) => !handled.has(type)))],
  };
}

const isGreen = (verdict: ReturnType<typeof lockstepVerdict>): boolean =>
  verdict.walkedAll && verdict.missing.length === 0;

describe("worker message lockstep", () => {
  const adapterDir = dirname(fileURLToPath(import.meta.url));
  const clientSource = readFileSync(resolve(adapterDir, "..", "engine-worker-client.ts"), "utf8");
  const workerSource = readFileSync(resolve(adapterDir, "..", "engine-worker.ts"), "utf8");

  it("posts only message types the worker's dispatch switch handles", () => {
    const verdict = lockstepVerdict(clientSource, workerSource);

    // Each leg separately, so a failure names which one fell.
    expect(verdict.walkedAll).toBe(true);
    expect(verdict.reach).toEqual(["previewInteraction", "submitInteraction"]);
    expect(verdict.missing).toEqual([]);
  });

  const handledFirst = handledMessageTypes(workerSource)[0];
  const outsideDispatch = Array.from(
    workerSource.matchAll(/case "([A-Za-z0-9_]+)":/g),
    (m) => m[1],
  ).find((label) => !handledMessageTypes(workerSource).includes(label));

  const insertIntoDispatchBody = (source: string, snippet: string): string => {
    const at = source.indexOf("switch (msg.type)");
    const open = source.indexOf("{", at);
    return `${source.slice(0, open + 1)}${snippet}${source.slice(open + 1)}`;
  };

  it.each([
    [
      "a worker case misspelled relative to the posted literal",
      () => ({
        client: clientSource,
        worker: workerSource.replace(`case "${handledFirst}":`, `case "${handledFirst}Xx":`),
      }),
    ],
    [
      "an untyped post with no worker case",
      () => ({
        client: `${clientSource}\nvoid this.request({ type: "ghostUntypedMessage" });\n`,
        worker: workerSource,
      }),
    ],
    [
      "a posted type equal to a case in an unrelated switch",
      () => ({
        client: `${clientSource}\nvoid this.request<void>({ type: "${outsideDispatch}" });\n`,
        worker: workerSource,
      }),
    ],
    [
      "a case reachable only inside a nested switch",
      () => ({
        client: `${clientSource}\nvoid this.request<void>({ type: "nestedGhost" });\n`,
        worker: insertIntoDispatchBody(
          workerSource,
          [
            "",
            '      case "nestedGhostOuter": {',
            "        switch (msg.id) {",
            '          case "nestedGhost": {',
            "            break;",
            "          }",
            "        }",
            "        break;",
            "      }",
          ].join("\n"),
        ),
      }),
    ],
    [
      "a request call site the extractor cannot read",
      () => ({
        client: `${clientSource}\nvoid this.request<void>(unreadableMessage);\n`,
        worker: workerSource,
      }),
    ],
    [
      "a request-prefixed identifier paired with an unreadable call site",
      () => ({
        client:
          `${clientSource}\n` +
          `this.requestQueue.push({ type: "${handledFirst}" });\n` +
          `void this.request<void>(hiddenMessage);\n`,
        worker: workerSource,
      }),
    ],
  ])("refuses %s", (_name, mutate) => {
    const { client, worker } = mutate();

    // A mutation that changed nothing would pass as a silent no-op, so the control asserts it
    // landed before it asserts what it produced.
    expect(client !== clientSource || worker !== workerSource).toBe(true);
    expect(isGreen(lockstepVerdict(client, worker))).toBe(false);
  });
});

// ── The preview envelope's FIELD names, both ends. ───────────────────────────────────────────
//
// `request()` takes a `Record<string, unknown>`, so nothing relates the keys `EngineWorkerClient`
// posts to the ones `engine-worker.ts` reads off `msg`. The row above compares only the `type`
// literal. This one runs the REAL client method against a stubbed `Worker` and compares the keys
// it actually posts against the reads that case performs, taken from the worker module as text.
// It executes the client body only.

/** The distinct `msg.<field>` names one dispatch case reads. */
function caseFieldReads(workerSource: string, type: string): string[] {
  const body = dispatchSwitchBody(workerSource);
  if (body === null) return [];
  const at = body.indexOf(`case "${type}":`);
  if (at < 0) return [];
  let depth = 0;
  for (let i = body.indexOf("{", at); i < body.length; i++) {
    if (body[i] === "{") depth++;
    else if (body[i] === "}" && --depth === 0) {
      const reads = body.slice(at, i).matchAll(/\bmsg\.([A-Za-z0-9_]+)/g);
      return [...new Set(Array.from(reads, (m) => m[1]))];
    }
  }
  return [];
}

/** Captures what the client posts and lets a test reply, like the worker-client suite's stub. */
class StubWorker {
  static last: StubWorker | undefined;
  onmessage: ((e: MessageEvent) => void) | null = null;
  onerror: ((e: ErrorEvent) => void) | null = null;
  readonly posted: Array<Record<string, unknown>> = [];

  constructor() {
    StubWorker.last = this;
  }

  postMessage(msg: Record<string, unknown>): void {
    this.posted.push(msg);
  }

  terminate(): void {}

  replyResult(id: number, data: unknown): void {
    this.onmessage?.({ data: { type: "result", id, data } } as MessageEvent);
  }
}

describe("worker preview envelope", () => {
  const workerSource = readFileSync(
    resolve(dirname(fileURLToPath(import.meta.url)), "..", "engine-worker.ts"),
    "utf8",
  );

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("posts exactly the fields the worker's previewInteraction case reads", async () => {
    vi.stubGlobal("Worker", StubWorker);
    const { EngineWorkerClient: RealClient } = await vi.importActual<
      typeof import("../engine-worker-client")
    >("../engine-worker-client");
    const client = new RealClient();
    const worker = StubWorker.last!;

    const pending = client.previewInteraction(1, request);
    const posted = worker.posted[0];

    // Renaming a field on either side moves exactly one of these two sets.
    expect(new Set(Object.keys(posted))).toEqual(
      new Set(["type", ...caseFieldReads(workerSource, "previewInteraction")]),
    );
    expect(posted.actor).toBe(1);
    expect(posted.request).toEqual(request);

    worker.replyResult(posted.id as number, answer);

    await expect(pending).resolves.toEqual(answer);
    client.dispose();
  });
});

// ── Negotiated state-codec sessions ─────────────────────────────────────
//
// Sessions are decoder leases over one engine endpoint. These rows pin
// ownership: ids are per-open, one close never affects another session or a
// replacement runtime, and every facade releases only what it opened.

describe("WasmAdapter state-codec sessions", () => {
  let subject: WasmAdapter;

  beforeEach(() => {
    vi.clearAllMocks();
    subject = new WasmAdapter();
    getGameStateJs.mockReturnValue(buildGameState());
    getLegalActionsForViewerJs.mockReturnValue({ actions: [], autoPassRecommended: false });
  });

  afterEach(() => {
    vi.useRealTimers();
    subject.dispose();
  });

  it("binds a lease to its worker session and closes it exactly once", async () => {
    mockWorkerClient.openStateCodec.mockResolvedValueOnce(41);
    mockWorkerClient.decodeStateCodec.mockResolvedValueOnce({
      kind: "decoded",
      json: '{"type":"Pong"}',
    });
    await subject.initialize();

    const session = await subject.openStateCodecSession();
    expect(session).not.toBeNull();
    await expect(session!.decode(new Uint8Array([1, 2]))).resolves.toEqual({
      kind: "decoded",
      json: '{"type":"Pong"}',
    });
    expect(mockWorkerClient.decodeStateCodec).toHaveBeenCalledWith(41, expect.any(Uint8Array));

    session!.close();
    session!.close();
    expect(mockWorkerClient.closeStateCodec).toHaveBeenCalledOnce();
    expect(mockWorkerClient.closeStateCodec).toHaveBeenCalledWith(41);
    await expect(session!.decode(new Uint8Array([3]))).rejects
      .toBeInstanceOf(StateCodecLifecycleError);
  });

  // A worker decode in flight when the lease is released must not emit: the late
  // result is converted to a lifecycle rejection, the lease is released once,
  // and the rejection is observed by the caller (no unhandled rejection).
  async function pendingWorkerDecode() {
    mockWorkerClient.openStateCodec.mockResolvedValueOnce(41);
    let finishDecode!: (result: { kind: "decoded"; json: string }) => void;
    mockWorkerClient.decodeStateCodec.mockReturnValueOnce(
      new Promise((resolve) => { finishDecode = resolve; }),
    );
    await subject.initialize();
    const session = await subject.openStateCodecSession();
    expect(session).not.toBeNull();
    const outcome = session!.decode(new Uint8Array([1])).then(
      (value) => ({ resolvedWith: value }),
      (error: unknown) => error,
    );
    await vi.waitFor(() => expect(mockWorkerClient.decodeStateCodec).toHaveBeenCalledOnce());
    return { session: session!, finishDecode, outcome };
  }

  it("emits a worker decode that completes while the lease is still held", async () => {
    // Positive control: the same pending decode DOES emit when nothing released
    // the lease, so the rejection rows below are not a dead path.
    const { session, finishDecode, outcome } = await pendingWorkerDecode();
    finishDecode({ kind: "decoded", json: '{"type":"Pong"}' });
    await expect(outcome).resolves.toEqual({
      resolvedWith: { kind: "decoded", json: '{"type":"Pong"}' },
    });
    expect(mockWorkerClient.closeStateCodec).not.toHaveBeenCalled();
    session.close();
    expect(mockWorkerClient.closeStateCodec).toHaveBeenCalledOnce();
  });

  it.each(["session close", "adapter dispose"] as const)(
    "rejects a worker decode that is pending when the %s releases the lease",
    async (how) => {
      const { session, finishDecode, outcome } = await pendingWorkerDecode();

      if (how === "session close") session.close();
      else subject.dispose();
      expect(mockWorkerClient.closeStateCodec).toHaveBeenCalledOnce();
      expect(mockWorkerClient.closeStateCodec).toHaveBeenCalledWith(41);

      finishDecode({ kind: "decoded", json: '{"type":"Pong"}' });
      const settled = await outcome;
      expect(settled).toBeInstanceOf(StateCodecLifecycleError);
      expect(settled).not.toHaveProperty("resolvedWith");
      // The release stays single even after the late result and a repeat close.
      session.close();
      expect(mockWorkerClient.closeStateCodec).toHaveBeenCalledOnce();
    },
  );

  it("disposing fallback facade A preserves facade B's retained baseline", async () => {
    const sibling = new WasmAdapter();
    mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
    await subject.initialize();
    mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
    await sibling.initialize();
    const aFree = vi.fn();
    const bFree = vi.fn();
    const bClone = { json: () => '{"viewer":"B"}', free: vi.fn() };
    stateCodecDecodeJs
      .mockReturnValueOnce({ json: () => '{"viewer":"A"}', free: aFree, clone_handle: vi.fn() })
      .mockReturnValueOnce({ json: () => '{"viewer":"B"}', free: bFree, clone_handle: () => bClone })
      .mockImplementationOnce((_frame, baseline) => {
        // B's second decode must receive B's retained clone, not A or null.
        expect(baseline).toBe(bClone);
        return { json: () => '{"viewer":"B","turn":2}', free: vi.fn(), clone_handle: vi.fn() };
      });
    try {
      const first = await subject.openStateCodecSession();
      const second = await sibling.openStateCodecSession();
      expect(await first!.decode(new Uint8Array([1]))).toEqual({ kind: "decoded", json: '{"viewer":"A"}' });
      expect(await second!.decode(new Uint8Array([2]))).toEqual({ kind: "decoded", json: '{"viewer":"B"}' });
      subject.dispose();
      expect(await second!.decode(new Uint8Array([3]))).toEqual({ kind: "decoded", json: '{"viewer":"B","turn":2}' });
      expect(aFree).toHaveBeenCalledOnce();
      expect(bFree).toHaveBeenCalledOnce();
      await expect(first!.decode(new Uint8Array([4]))).rejects.toBeInstanceOf(StateCodecLifecycleError);
    } finally {
      sibling.dispose();
    }
  });

  it("releases this facade's leases on dispose", async () => {
    mockWorkerClient.openStateCodec.mockResolvedValueOnce(1).mockResolvedValueOnce(2);
    await subject.initialize();
    await subject.openStateCodecSession();
    await subject.openStateCodecSession();

    subject.dispose();

    expect(mockWorkerClient.closeStateCodec).toHaveBeenCalledTimes(2);
    expect(mockWorkerClient.closeStateCodec).toHaveBeenCalledWith(1);
    expect(mockWorkerClient.closeStateCodec).toHaveBeenCalledWith(2);
  });

  it("closes a session that opens after disposal won the race", async () => {
    let finishOpen!: (sessionId: number) => void;
    mockWorkerClient.openStateCodec.mockReturnValueOnce(
      new Promise<number>((resolve) => { finishOpen = resolve; }),
    );
    await subject.initialize();

    const opening = subject.openStateCodecSession();
    await vi.waitFor(() => expect(mockWorkerClient.openStateCodec).toHaveBeenCalledOnce());
    subject.dispose();
    finishOpen(7);

    await expect(opening).resolves.toBeNull();
    expect(mockWorkerClient.closeStateCodec).toHaveBeenCalledWith(7);
  });

  it("opens and decodes through the shared main-thread fallback", async () => {
    mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
    const free = vi.fn();
    stateCodecDecodeJs.mockReturnValue({
      json: () => '{"type":"StateUpdate"}',
      free,
      clone_handle: vi.fn(),
    });
    await subject.initialize();
    expect(subject.getEngineClient()).toBeNull();

    const session = await subject.openStateCodecSession();
    expect(session).not.toBeNull();
    await expect(session!.decode(new Uint8Array([1]))).resolves.toEqual({
      kind: "decoded",
      json: '{"type":"StateUpdate"}',
    });
    expect(stateCodecDecodeJs).toHaveBeenCalledOnce();
    session!.close();
    await expect(session!.decode(new Uint8Array([2]))).rejects
      .toBeInstanceOf(StateCodecLifecycleError);
  });

  it("applies the caller-visible decode deadline on the main-thread fallback", async () => {
    mockWorkerClient.initialize.mockRejectedValueOnce(new Error("worker unavailable"));
    stateCodecDecodeJs.mockReturnValue({ json: () => "{}", free: vi.fn(), clone_handle: vi.fn() });
    await subject.initialize();
    const session = await subject.openStateCodecSession();

    // Block the shared runtime queue behind one never-settling engine read.
    let releaseQueue!: (state: unknown) => void;
    getGameStateJs.mockReturnValueOnce(new Promise((resolve) => { releaseQueue = resolve; }));
    void subject.getState();

    vi.useFakeTimers();
    // Observe before the timer fires: the rejection lands inside the advance.
    const outcome = session!.decode(new Uint8Array([1])).then(
      () => "resolved",
      (error: unknown) => error,
    );
    await vi.advanceTimersByTimeAsync(STATE_CODEC_DECODE_TIMEOUT_MS + 1);
    await expect(outcome).resolves.toMatchObject({ message: expect.stringMatching(/timed out/) });
    session!.close();

    releaseQueue(buildGameState());
    await vi.advanceTimersByTimeAsync(0);
  });
});

// ── Decode-frame transfer across the real worker boundary ───────────────
//
// The rest of this file mocks `EngineWorkerClient`, so this row loads the real
// module (bypassing the mock) to prove the transfer list itself.

describe("EngineWorkerClient codec frame transfer", () => {
  class TransferWorker {
    static last: TransferWorker | null = null;
    onmessage: ((event: MessageEvent) => void) | null = null;
    onerror: ((event: ErrorEvent) => void) | null = null;
    readonly ids: number[] = [];

    constructor() {
      TransferWorker.last = this;
    }

    postMessage(message: Record<string, unknown>, transfer?: Transferable[]): void {
      // A real transfer detaches the source buffer; structuredClone emulates it.
      structuredClone(message, { transfer });
      this.ids.push(message.id as number);
      const id = message.id as number;
      const result = { kind: "decoded", json: '{"type":"Pong"}' };
      queueMicrotask(() => {
        this.onmessage?.({ data: { type: "result", id, data: result } } as MessageEvent);
      });
    }

    terminate(): void {}
  }

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("transfers the whole frame buffer so the caller's view is detached", async () => {
    vi.stubGlobal("Worker", TransferWorker);
    const actual = await vi.importActual<typeof import("../engine-worker-client")>(
      "../engine-worker-client",
    );
    const client = new actual.EngineWorkerClient();
    try {
      const frame = new TextEncoder().encode(JSON.stringify({ type: "Full", data: {} }));
      const pending = client.decodeStateCodec(3, frame);

      expect(TransferWorker.last!.ids).toHaveLength(1);
      // Ownership moved: the caller can no longer read through its view.
      expect(frame.byteLength).toBe(0);
      await expect(pending).resolves.toEqual({ kind: "decoded", json: '{"type":"Pong"}' });
    } finally {
      client.dispose();
    }
  });

  it("rejects decode at 60s, ignores its late reply, and still accepts a subsequent request", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("Worker", StubWorker);
    const actual = await vi.importActual<typeof import("../engine-worker-client")>("../engine-worker-client");
    const client = new actual.EngineWorkerClient();
    try {
      const outcome = client.decodeStateCodec(3, new TextEncoder().encode("frame")).catch((error: unknown) => error);
      const worker = StubWorker.last!;
      const firstId = worker.posted[0].id as number;
      await vi.advanceTimersByTimeAsync(60_000);
      expect(await outcome).toMatchObject({ message: expect.stringContaining("timed out") });
      worker.replyResult(firstId, { kind: "decoded", json: "late" });
      const next = client.decodeStateCodec(4, new TextEncoder().encode("next"));
      worker.replyResult(worker.posted[1].id as number, { kind: "decoded", json: "live" });
      await expect(next).resolves.toEqual({ kind: "decoded", json: "live" });
    } finally {
      client.dispose();
      vi.useRealTimers();
    }
  });

  it("rejects synchronous post failure without a lingering timeout or poisoned requests", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("Worker", StubWorker);
    const actual = await vi.importActual<typeof import("../engine-worker-client")>("../engine-worker-client");
    const client = new actual.EngineWorkerClient();
    try {
      const worker = StubWorker.last!;
      const post = vi.spyOn(worker, "postMessage").mockImplementationOnce(() => { throw new Error("post failed"); });
      await expect(client.decodeStateCodec(1, new Uint8Array([1]))).rejects.toThrow("post failed");
      expect(vi.getTimerCount()).toBe(0);
      post.mockRestore();
      const next = client.openStateCodec();
      await vi.advanceTimersByTimeAsync(60_001);
      worker.replyResult(worker.posted[0].id as number, 5);
      await expect(next).resolves.toBe(5); // Open responses are never orphaned by a watchdog.
    } finally {
      client.dispose();
      vi.useRealTimers();
    }
  });
});
