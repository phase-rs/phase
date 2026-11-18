// A persisted zustand store captures its storage when this file's imports are
// evaluated, so the working-localStorage install has to precede them.
import "../../../test/helpers/persistedStorage";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  AiActionProposal,
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
import { dispatchAction, dispatchAiActionProposal, isDispatchIdle, undoLastAction } from "../../dispatch";
import { createAIController } from "../aiController";

const catalogMock = vi.hoisted(() => ({ loadProviderCatalog: vi.fn() }));
vi.mock("../../../services/llm/catalog", () => catalogMock);
vi.mock("../../../services/llm/llmClient", () => ({ executeLlmRequest: vi.fn() }));
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

function logEntry(turn: number, text: string): GameLogEntry {
  return {
    seq: 0,
    turn,
    phase: "PreCombatMain",
    category: "Stack",
    segments: [{ type: "Text", value: text }],
    presentation: { importance: "Essential", tone: "Neutral", boundary: "None", visibility: "Public" },
  };
}

const EARLIER_PLAYS = [
  logEntry(2, "Player 1 casts Sensei's Divining Top"),
  logEntry(3, "Player 2 casts Sol Ring"),
];
// The line the engine logs for a pass (category Turn), naming who receives priority.
const HUMAN_PASS = { ...logEntry(3, "Priority passes to Player 2"), category: "Turn" as const };
const LORAN_PLAY = [
  logEntry(3, "Player 2 casts Loran of the Third Path"),
  logEntry(3, "Sensei's Divining Top is destroyed"),
];
const LORAN_PROPOSAL: AiActionProposal = {
  token: "loran",
  semanticOwner: AI_SEAT,
  actor: AI_SEAT,
  // Loran's destroy is an enters trigger, so the spell is cast with no target.
  action: { type: "CastSpell", data: { object_id: 40, card_id: 7, targets: [] } },
};

/** The store's log as text, oldest first. */
function loggedLines(): string[] {
  return useGameStore.getState().logHistory.map((entry) =>
    entry.segments.map((segment) => (segment.type === "Text" ? segment.value : "")).join(""),
  );
}

describe("an LLM seat's history after an undo", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(1);
    catalogMock.loadProviderCatalog.mockReset();
    catalogMock.loadProviderCatalog.mockResolvedValue(CATALOG);
    // Instant animations: no presentation waits between an action and its commit.
    usePreferencesStore.setState({ animationSpeedMultiplier: 0 });
    useLlmStore.setState({
      profiles: [],
      seatBindings: {},
      defaultOpponentProfileId: null,
      draftEnabled: false,
      draftProfileId: null,
    });
    useGameStore.getState().reset();
  });

  afterEach(() => {
    useGameStore.getState().reset();
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it("leaves the undone plays out of the seat's next decision request", async () => {
    const profileId = useLlmStore
      .getState()
      .addProfile({ name: "Test", model: "gpt-5", apiKey: "k", enabled: true });
    useLlmStore.getState().bindSeat(0, profileId);

    let live: GameState = HUMAN_TO_ACT;
    const buildLlmDecisionRequest = vi.fn<
      (
        difficulty: string,
        playerId: number,
        endpointJson: string,
        historyJson: string,
        settingsRevision: number,
        clock: LlmClockReading,
      ) => Promise<LlmDecisionRequestResult>
    >(
      // Nothing is sent: the history the request was built from is the point.
      async () => ({ errorKind: { kind: "unsupportedDecision" } }),
    );
    const adapter = buildEngineAdapterMock(HUMAN_TO_ACT, {
      getState: vi.fn(async () => live),
      exportPersistenceState: vi.fn(async () =>
        JSON.stringify({ state: live, precast_shortcut_runtime: null }),
      ),
      restoreState: vi.fn((restored: PersistedGameState) => {
        live = persistedGameStateView(restored);
      }),
      buildLlmDecisionRequest,
      getAiActionProposalFromLlmResponse: vi.fn(),
    });
    useGameStore.setState({ adapter, gameMode: "ai", aiSeatIds: [AI_SEAT] });
    // The game so far, committed the way every action's log lines are.
    useGameStore.getState().commitEngineSnapshot(await adapter.getSnapshot(), {
      logEntries: EARLIER_PLAYS,
    });

    // The human passes priority: an undoable action, so it is checkpointed.
    adapter.submitAction.mockImplementationOnce(async () => {
      live = AI_TO_ACT;
      return { events: [], log_entries: [HUMAN_PASS] };
    });
    await dispatchAction({ type: "PassPriority" }, HUMAN);
    expect(useGameStore.getState().stateHistory).toHaveLength(1);

    // The LLM seat casts Loran of the Third Path and destroys the human's Top.
    adapter.submitAiActionProposal.mockImplementationOnce(async () => {
      live = HUMAN_TO_ACT;
      return { status: "applied", result: { events: [], log_entries: LORAN_PLAY } };
    });
    await expect(dispatchAiActionProposal(LORAN_PROPOSAL)).resolves.toEqual({ status: "applied" });
    expect(loggedLines()).toEqual([
      "Player 1 casts Sensei's Divining Top",
      "Player 2 casts Sol Ring",
      "Priority passes to Player 2",
      "Player 2 casts Loran of the Third Path",
      "Sensei's Divining Top is destroyed",
    ]);

    // Undo takes back the pass, and the Loran play with it. It is pressed
    // with nothing in flight; a play still animating when Undo is pressed is
    // covered in `aiControllerLlmUndoInFlight.test.ts` (#11014).
    expect(isDispatchIdle()).toBe(true);
    await undoLastAction();
    expect(adapter.restoreState).toHaveBeenCalledOnce();
    expect(useGameStore.getState().gameState).toEqual(HUMAN_TO_ACT);

    // The human passes again, and the LLM seat has a decision to make.
    adapter.submitAction.mockImplementationOnce(async () => {
      live = AI_TO_ACT;
      return { events: [], log_entries: [HUMAN_PASS] };
    });
    await dispatchAction({ type: "PassPriority" }, HUMAN);

    const controller = createAIController({
      seats: [{ playerId: AI_SEAT, difficulty: "Medium", llmSeatIndex: 0 }],
    });
    controller.start();
    await vi.waitFor(() => expect(buildLlmDecisionRequest).toHaveBeenCalled());
    controller.dispose();

    // The plays before the undone pass, then the pass made again. Seqs 2 to 4
    // named the undone pass and the Loran play, and are not reused.
    const historyJson = buildLlmDecisionRequest.mock.calls[0][3];
    expect(JSON.parse(historyJson)).toEqual([
      { ...EARLIER_PLAYS[0], seq: 0 },
      { ...EARLIER_PLAYS[1], seq: 1 },
      { ...HUMAN_PASS, seq: 5 },
    ]);
  });
});
