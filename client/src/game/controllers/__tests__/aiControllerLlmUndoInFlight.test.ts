// A persisted zustand store captures its storage when this file's imports are
// evaluated, so the working-localStorage install has to precede them.
import "../../../test/helpers/persistedStorage";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  AiActionProposal,
  AiLlmProposalResult,
  GameEvent,
  GameLogEntry,
  GameState,
  LlmClockReading,
  LlmDecisionRequestResult,
  PersistedGameState,
} from "../../../adapter/types";
import { persistedGameStateView } from "../../../adapter/types";
import type { LlmProviderCatalogEntry } from "../../../services/llm/types";
import { useGameStore } from "../../../stores/gameStore";
import { useLlmStore } from "../../../stores/llmStore";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import { buildEngineAdapterMock } from "../../../test/factories/engineAdapterFactory";
import { gameStateFactory } from "../../../test/factories/gameStateFactory";
import {
  abandonPendingDispatches,
  dispatchAction,
  dispatchAiActionProposal,
  isDispatchIdle,
  undoLastAction,
} from "../../dispatch";
import { createAIController, type AIController } from "../aiController";

const catalogMock = vi.hoisted(() => ({ loadProviderCatalog: vi.fn() }));
vi.mock("../../../services/llm/catalog", () => catalogMock);
const llmClientMock = vi.hoisted(() => ({ executeLlmRequest: vi.fn() }));
vi.mock("../../../services/llm/llmClient", () => llmClientMock);
vi.mock("../../engineRecovery", () => ({
  attemptStateRehydrate: async () => false,
  isEnginePanic: () => false,
  notifyEngineLost: vi.fn(),
  routePanic: async () => {},
}));
// `debugLog` writes into the game log, so the controller's own diagnostics
// would otherwise sit in the very history under test.
vi.mock("../../debugLog", () => ({ debugLog: vi.fn() }));

const CATALOG: LlmProviderCatalogEntry[] = [{
  provider: "OpenAi", value: "OpenAi", displayName: "OpenAI", defaultBaseUrl: null,
  defaultModel: "gpt-5", requiresApiKey: true, apiKeyUrl: "", models: [], decisionContexts: ["selfContained"],
}];

const HUMAN = 0;
const AI_SEAT = 1;

const HUMAN_TO_ACT = gameStateFactory
  .withPlayers(HUMAN, AI_SEAT)
  .onTurn(3)
  .activePlayer(HUMAN)
  .priority(HUMAN)
  .build();
const AI_TO_ACT = gameStateFactory
  .withPlayers(HUMAN, AI_SEAT)
  .onTurn(3)
  .activePlayer(AI_SEAT)
  .priority(AI_SEAT)
  .build();

function logEntry(text: string): GameLogEntry {
  return {
    seq: 0,
    turn: 3,
    phase: "PreCombatMain",
    category: "Stack",
    segments: [{ type: "Text", value: text }],
    presentation: { importance: "Essential", tone: "Neutral", boundary: "None", visibility: "Public" },
  };
}

const EARLIER_PLAYS = [
  logEntry("Player 1 casts Sensei's Divining Top"),
  logEntry("Player 2 casts Sol Ring"),
];
// The line the engine logs for a pass (category Turn), naming who receives priority.
const HUMAN_PASS = { ...logEntry("Priority passes to Player 2"), category: "Turn" as const };
const LORAN_PLAY = [
  logEntry("Player 2 casts Loran of the Third Path"),
  logEntry("Sensei's Divining Top is destroyed"),
];
const LORAN_PROPOSAL: AiActionProposal = {
  token: "loran",
  semanticOwner: AI_SEAT,
  actor: AI_SEAT,
  // Loran's destroy is an enters trigger, so the spell is cast with no target.
  action: { type: "CastSpell", data: { object_id: 40, card_id: 7, targets: [] } },
};
/** Normalizes to an animation step, so the play that carries it waits out a real animation. */
const LIFE_LOSS: GameEvent = { type: "LifeChanged", data: { player_id: HUMAN, amount: -2 } };

const HTTP_SPEC = {
  url: "https://api.example.test/v1/chat/completions",
  method: "POST",
  headers: [],
  body: "{}",
};
const REQUEST: LlmDecisionRequestResult = {
  fingerprint: "fp-1",
  optionCount: 2,
  promptChars: 10,
  request: HTTP_SPEC,
};
/** The engine's verdict on a reply to a decision an undo has replaced. */
const STALE_REFUSAL: AiLlmProposalResult = {
  proposal: null,
  error: "the decision is no longer current",
  errorKind: { kind: "staleDecision" },
};

/**
 * A solo game against an LLM seat whose engine is a variable: `submitAction`,
 * `submitAiActionProposal` and `restoreState` change it as they are called, as
 * the real engine applies requests in call order, and every read sees it as it
 * stands, unless `holdSnapshots` is holding the reads.
 */
async function startGame(buildResult: LlmDecisionRequestResult = REQUEST) {
  const profileId = useLlmStore
    .getState()
    .addProfile({ name: "Test", model: "gpt-5", apiKey: "k", enabled: true });
  useLlmStore.getState().bindSeat(0, profileId);

  let live: GameState = HUMAN_TO_ACT;
  let snapshotHold: Promise<void> | null = null;
  const buildLlmDecisionRequest = vi.fn<
    (
      difficulty: string,
      playerId: number,
      endpointJson: string,
      historyJson: string,
      settingsRevision: number,
      clock: LlmClockReading,
    ) => Promise<LlmDecisionRequestResult>
  >(async () => buildResult);
  const adapter = buildEngineAdapterMock(HUMAN_TO_ACT, {
    getState: vi.fn(async () => {
      if (snapshotHold) await snapshotHold;
      return live;
    }),
    exportPersistenceState: vi.fn(async () =>
      JSON.stringify({ state: live, precast_shortcut_runtime: null }),
    ),
    restoreState: vi.fn((restored: PersistedGameState) => {
      live = persistedGameStateView(restored);
    }),
    buildLlmDecisionRequest,
    getAiActionProposalFromLlmResponse: vi.fn(async () => STALE_REFUSAL),
  });
  useGameStore.setState({ adapter, gameMode: "ai", aiSeatIds: [AI_SEAT] });
  // The game so far, committed the way every action's log lines are.
  useGameStore.getState().commitEngineSnapshot(await adapter.getSnapshot(), {
    logEntries: EARLIER_PLAYS,
  });

  /** The human passes priority to the LLM seat: an undoable, checkpointed action. */
  const humanPasses = async () => {
    adapter.submitAction.mockImplementationOnce(async () => {
      live = AI_TO_ACT;
      return { events: [], log_entries: [HUMAN_PASS] };
    });
    await dispatchAction({ type: "PassPriority" }, HUMAN);
  };
  /** The LLM seat's play: Loran of the Third Path, which hands the human priority. */
  const loranResolves = (events: GameEvent[]) => {
    adapter.submitAiActionProposal.mockImplementationOnce(async () => {
      live = HUMAN_TO_ACT;
      return { status: "applied", result: { events, log_entries: LORAN_PLAY } };
    });
  };
  /**
   * Hold every snapshot read until the returned function lets them go: an
   * undo pressed meanwhile has restored the engine but not yet committed.
   */
  const holdSnapshots = () => {
    let release!: () => void;
    snapshotHold = new Promise<void>((resolve) => {
      release = resolve;
    });
    return () => {
      snapshotHold = null;
      release();
    };
  };
  return { adapter, buildLlmDecisionRequest, humanPasses, loranResolves, holdSnapshots };
}

/** Every controller a test starts, disposed after it whatever its outcome. */
const controllers: AIController[] = [];

function startController(): AIController {
  const controller = createAIController({
    seats: [{ playerId: AI_SEAT, difficulty: "Medium", llmSeatIndex: 0 }],
  });
  controllers.push(controller);
  controller.start();
  return controller;
}

/** Each provider call stays out until the test answers it, oldest first. */
const providerReplies: Array<(response: { status: number; body: string }) => void> = [];

describe("an LLM seat's decision when Undo is pressed", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(1);
    catalogMock.loadProviderCatalog.mockReset();
    catalogMock.loadProviderCatalog.mockResolvedValue(CATALOG);
    providerReplies.length = 0;
    llmClientMock.executeLlmRequest.mockReset();
    llmClientMock.executeLlmRequest.mockImplementation(
      () => new Promise((resolve) => providerReplies.push(resolve)),
    );
    // A real animation window, where a play still in flight waits.
    usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
    useLlmStore.setState({
      profiles: [],
      seatBindings: {},
      defaultOpponentProfileId: null,
      draftEnabled: false,
      draftProfileId: null,
    });
    abandonPendingDispatches();
    useGameStore.getState().reset();
  });

  afterEach(() => {
    for (const controller of controllers.splice(0)) controller.dispose();
    useGameStore.getState().reset();
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it("leaves a play still animating at the undo out of the seat's next decision request", async () => {
    const { buildLlmDecisionRequest, humanPasses, loranResolves } = await startGame({
      // Nothing is sent: the history the request was built from is the point.
      errorKind: { kind: "unsupportedDecision" },
    });
    await humanPasses();
    expect(useGameStore.getState().stateHistory).toHaveLength(1);

    // The LLM seat's Loran play reaches the engine and waits out its animation.
    loranResolves([LIFE_LOSS]);
    const loran = dispatchAiActionProposal(LORAN_PROPOSAL);
    await vi.advanceTimersByTimeAsync(0);
    expect(isDispatchIdle()).toBe(false);

    // Undo takes back the pass, and the Loran play with it.
    await undoLastAction();
    expect(useGameStore.getState().gameState).toEqual(HUMAN_TO_ACT);

    // Loran's animation ends after the undo.
    await vi.runAllTimersAsync();
    await loran;

    // The human passes again, and the LLM seat has a decision to make.
    await humanPasses();
    const controller = startController();
    await vi.waitFor(() => expect(buildLlmDecisionRequest).toHaveBeenCalled());
    controller.dispose();

    // The plays before the undone pass, then the pass made again. Seq 2 named
    // the undone pass and is not reused.
    const historyJson = buildLlmDecisionRequest.mock.calls[0][3];
    expect(JSON.parse(historyJson)).toEqual([
      { ...EARLIER_PLAYS[0], seq: 0 },
      { ...EARLIER_PLAYS[1], seq: 1 },
      { ...HUMAN_PASS, seq: 3 },
    ]);
  });

  it("never counts a reply that arrives during an undo against the seat's provider", async () => {
    const { adapter, humanPasses, holdSnapshots } = await startGame();
    startController();

    // Three times over, the seat asks its provider, and the human undoes the
    // pass while the reply is out. The reply lands after the restore and
    // before the undo commits, where the engine would refuse it as stale.
    for (let round = 1; round <= 3; round += 1) {
      await humanPasses();
      await vi.advanceTimersByTimeAsync(0);
      expect(llmClientMock.executeLlmRequest).toHaveBeenCalledTimes(round);

      const releaseSnapshots = holdSnapshots();
      const undoing = undoLastAction();
      await vi.advanceTimersByTimeAsync(0);
      expect(adapter.restoreState).toHaveBeenCalledTimes(round);

      providerReplies.shift()!({ status: 200, body: "{}" });
      await vi.advanceTimersByTimeAsync(0);
      // The undo ended the decision, so its reply is never even read.
      expect(adapter.getAiActionProposalFromLlmResponse).not.toHaveBeenCalled();

      releaseSnapshots();
      await undoing;
      expect(useGameStore.getState().gameState).toEqual(HUMAN_TO_ACT);
    }

    // Three refusals in a row would have given the provider up for the game.
    // The seat's next decision still goes to it.
    await humanPasses();
    await vi.advanceTimersByTimeAsync(0);
    expect(llmClientMock.executeLlmRequest).toHaveBeenCalledTimes(4);
  });

  it("builds nothing for a decision still waiting on the provider catalog when the undo restores", async () => {
    const { buildLlmDecisionRequest, humanPasses, holdSnapshots } = await startGame();
    let catalogLoaded!: (catalog: LlmProviderCatalogEntry[]) => void;
    catalogMock.loadProviderCatalog.mockReturnValue(
      new Promise<LlmProviderCatalogEntry[]>((resolve) => {
        catalogLoaded = resolve;
      }),
    );
    startController();

    // The seat's decision begins, and waits on the catalog.
    await humanPasses();
    await vi.advanceTimersByTimeAsync(0);
    expect(catalogMock.loadProviderCatalog).toHaveBeenCalledOnce();

    // The undo restores the engine to the human's decision, but has not
    // committed: the store still shows the seat's decision.
    const releaseSnapshots = holdSnapshots();
    const undoing = undoLastAction();
    await vi.advanceTimersByTimeAsync(0);
    expect(useGameStore.getState().gameState).toEqual(AI_TO_ACT);
    // No decision starts while the undo runs.
    expect(catalogMock.loadProviderCatalog).toHaveBeenCalledOnce();

    // The catalog arrives. A build now would render the restored engine's
    // decision, the human's, from the human's view.
    catalogLoaded(CATALOG);
    await vi.advanceTimersByTimeAsync(0);
    expect(buildLlmDecisionRequest).not.toHaveBeenCalled();

    releaseSnapshots();
    await undoing;
    await vi.advanceTimersByTimeAsync(0);
    expect(useGameStore.getState().gameState).toEqual(HUMAN_TO_ACT);
    expect(buildLlmDecisionRequest).not.toHaveBeenCalled();
    expect(llmClientMock.executeLlmRequest).not.toHaveBeenCalled();
  });

  it("asks the seat's decision again when the undo's restore fails", async () => {
    const { adapter, humanPasses } = await startGame();
    startController();
    await humanPasses();
    await vi.advanceTimersByTimeAsync(0);
    expect(llmClientMock.executeLlmRequest).toHaveBeenCalledOnce();

    // The undo ends the decision under way, then fails: the engine, and the
    // seat's decision, stay as they were.
    adapter.restoreState.mockImplementationOnce(() => Promise.reject(new Error("restore failed")));
    await undoLastAction();
    expect(useGameStore.getState().undoInFlight).toBe(false);
    expect(useGameStore.getState().gameState).toEqual(AI_TO_ACT);

    await vi.advanceTimersByTimeAsync(0);
    expect(llmClientMock.executeLlmRequest).toHaveBeenCalledTimes(2);
  });
});
