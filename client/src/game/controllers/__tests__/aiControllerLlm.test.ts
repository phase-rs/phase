// A persisted zustand store captures its storage when this file's imports are
// evaluated, so the working-localStorage install has to precede them.
import "../../../test/helpers/persistedStorage";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  AiActionProposal,
  AiLlmProposalResult,
  GameAction,
  GameState,
  LlmDecisionRequestResult,
  WaitingFor,
} from "../../../adapter/types";
import { buildGameState } from "../../../test/factories/gameStateFactory";

const dispatchMocks = vi.hoisted(() => ({
  dispatchAiActionProposal: vi.fn<
    (proposal: AiActionProposal) => Promise<{ status: "applied" | "stale" }>
  >(),
}));
const llmMocks = vi.hoisted(() => ({
  executeLlmRequest: vi.fn<() => Promise<string>>(),
}));

vi.mock("../../dispatch", () => ({
  dispatchAiActionProposal: dispatchMocks.dispatchAiActionProposal,
}));
vi.mock("../../engineRecovery", () => ({
  attemptStateRehydrate: async () => false,
  isEnginePanic: () => false,
  notifyEngineLost: () => {},
  routePanic: async () => {},
}));
vi.mock("../../debugLog", () => ({ debugLog: vi.fn() }));
vi.mock("../../../services/llm/llmClient", () => ({
  executeLlmRequest: llmMocks.executeLlmRequest,
}));

interface TestAdapter {
  getAiActionProposal?: (difficulty: string, playerId: number) => Promise<AiActionProposal | null>;
  getAiTacticalActionProposal?: (
    difficulty: string,
    playerId: number,
  ) => Promise<AiActionProposal | null>;
  buildLlmDecisionRequest?: (
    difficulty: string,
    playerId: number,
    endpointJson: string,
    historyJson: string,
  ) => Promise<LlmDecisionRequestResult | null>;
  getAiActionProposalFromLlmResponse?: (
    playerId: number,
    fingerprint: string,
    provider: string,
    responseBody: string,
  ) => Promise<AiLlmProposalResult | null>;
}

let storeState: {
  gameState: GameState | null;
  waitingFor: WaitingFor | null;
  adapter: TestAdapter | null;
  logHistory: unknown[];
  gameSessionGeneration: number;
};

vi.mock("../../../stores/gameStore", () => ({
  useGameStore: {
    getState: () => storeState,
    subscribe: () => () => {},
  },
}));

import { createAIController } from "../aiController";
import { useLlmStore } from "../../../stores/llmStore";

const PASS = { type: "PassPriority" } as GameAction;
const CAST = { type: "ChoosePlayDraw", data: { play_first: true } } as unknown as GameAction;

function proposal(action: GameAction, token: string): AiActionProposal {
  return { token, semanticOwner: 1, actor: 1, action };
}

const HTTP_SPEC = {
  url: "https://api.example.test/v1/chat/completions",
  method: "POST",
  headers: [],
  body: "{}",
};

async function runOnce(): Promise<void> {
  await vi.advanceTimersByTimeAsync(1_000);
  for (let i = 0; i < 8; i += 1) await Promise.resolve();
  await vi.advanceTimersByTimeAsync(0);
  for (let i = 0; i < 8; i += 1) await Promise.resolve();
}

function bindSeatToProvider(): void {
  const id = useLlmStore
    .getState()
    .addProfile({ name: "Test", model: "gpt-5", apiKey: "k", enabled: true });
  useLlmStore.getState().bindSeat(0, id);
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.spyOn(Math, "random").mockReturnValue(1);
  dispatchMocks.dispatchAiActionProposal.mockReset();
  dispatchMocks.dispatchAiActionProposal.mockResolvedValue({ status: "applied" });
  llmMocks.executeLlmRequest.mockReset();
  useLlmStore.setState({
    profiles: [],
    seatBindings: {},
    draftEnabled: false,
    draftProfileId: null,
  });
  const state = buildGameState({
    waiting_for: { type: "Priority", data: { player: 1 } } as WaitingFor,
    priority_player: 1,
    stack: [],
  });
  storeState = {
    gameState: state,
    waitingFor: state.waiting_for,
    adapter: null,
    logHistory: [],
    gameSessionGeneration: 1,
  };
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("LLM-driven AI seats", () => {
  it("never touches the LLM path when no provider is bound to the seat", async () => {
    const heuristic = proposal(PASS, "heuristic");
    const buildLlmDecisionRequest = vi.fn();
    storeState.adapter = {
      getAiActionProposal: vi.fn(async () => heuristic),
      buildLlmDecisionRequest,
      getAiActionProposalFromLlmResponse: vi.fn(),
    };

    const controller = createAIController({
      seats: [{ playerId: 1, difficulty: "Medium", llmSeatIndex: 0 }],
    });
    controller.start();
    await runOnce();

    expect(buildLlmDecisionRequest).not.toHaveBeenCalled();
    expect(llmMocks.executeLlmRequest).not.toHaveBeenCalled();
    expect(dispatchMocks.dispatchAiActionProposal).toHaveBeenCalledWith(heuristic);
    controller.dispose();
  });

  it("submits the engine proposal the model's reply resolved to", async () => {
    bindSeatToProvider();
    const llmProposal = proposal(CAST, "llm-bound");
    const buildLlmDecisionRequest = vi.fn(async () => ({
      fingerprint: "fp-1",
      optionCount: 2,
      request: HTTP_SPEC,
    }));
    const getAiActionProposalFromLlmResponse = vi.fn(async () => ({
      proposal: llmProposal,
      reasoning: "develop the board",
    }));
    llmMocks.executeLlmRequest.mockResolvedValue('{"choice":1}');
    storeState.adapter = {
      getAiActionProposal: vi.fn(async () => proposal(PASS, "heuristic")),
      buildLlmDecisionRequest,
      getAiActionProposalFromLlmResponse,
    };

    const controller = createAIController({
      seats: [{ playerId: 1, difficulty: "Hard", llmSeatIndex: 0 }],
    });
    controller.start();
    await runOnce();

    // Difficulty reaches the engine so it can shape the persona and the amount
    // of the position the model is shown.
    expect(buildLlmDecisionRequest).toHaveBeenCalledWith("Hard", 1, expect.any(String), "[]");
    expect(getAiActionProposalFromLlmResponse).toHaveBeenCalledWith(
      1,
      "fp-1",
      "OpenAi",
      '{"choice":1}',
    );
    expect(dispatchMocks.dispatchAiActionProposal).toHaveBeenCalledWith(llmProposal);
    controller.dispose();
  });

  it("falls back to the engine AI when the provider call fails", async () => {
    bindSeatToProvider();
    const heuristic = proposal(PASS, "heuristic");
    llmMocks.executeLlmRequest.mockRejectedValue(new Error("network down"));
    storeState.adapter = {
      getAiActionProposal: vi.fn(async () => heuristic),
      buildLlmDecisionRequest: vi.fn(async () => ({
        fingerprint: "fp-1",
        optionCount: 2,
        request: HTTP_SPEC,
      })),
      getAiActionProposalFromLlmResponse: vi.fn(),
    };

    const controller = createAIController({
      seats: [{ playerId: 1, difficulty: "Medium", llmSeatIndex: 0 }],
    });
    controller.start();
    await runOnce();

    expect(dispatchMocks.dispatchAiActionProposal).toHaveBeenCalledWith(heuristic);
    controller.dispose();
  });

  it("falls back to the engine AI when the engine refuses the model's reply", async () => {
    bindSeatToProvider();
    const heuristic = proposal(PASS, "heuristic");
    llmMocks.executeLlmRequest.mockResolvedValue("I am not sure");
    storeState.adapter = {
      getAiActionProposal: vi.fn(async () => heuristic),
      buildLlmDecisionRequest: vi.fn(async () => ({
        fingerprint: "fp-1",
        optionCount: 2,
        request: HTTP_SPEC,
      })),
      getAiActionProposalFromLlmResponse: vi.fn(async () => ({
        proposal: null,
        error: "could not decode a choice from the LLM reply",
      })),
    };

    const controller = createAIController({
      seats: [{ playerId: 1, difficulty: "Medium", llmSeatIndex: 0 }],
    });
    controller.start();
    await runOnce();

    expect(dispatchMocks.dispatchAiActionProposal).toHaveBeenCalledWith(heuristic);
    controller.dispose();
  });

  it("falls back to the engine AI when the adapter has no LLM capability", async () => {
    bindSeatToProvider();
    const heuristic = proposal(PASS, "heuristic");
    storeState.adapter = { getAiActionProposal: vi.fn(async () => heuristic) };

    const controller = createAIController({
      seats: [{ playerId: 1, difficulty: "Medium", llmSeatIndex: 0 }],
    });
    controller.start();
    await runOnce();

    expect(llmMocks.executeLlmRequest).not.toHaveBeenCalled();
    expect(dispatchMocks.dispatchAiActionProposal).toHaveBeenCalledWith(heuristic);
    controller.dispose();
  });

  it("gives up on a provider that keeps failing and keeps the seat playing", async () => {
    bindSeatToProvider();
    const heuristic = proposal(PASS, "heuristic");
    llmMocks.executeLlmRequest.mockRejectedValue(new Error("provider down"));
    const buildLlmDecisionRequest = vi.fn(async () => ({
      fingerprint: "fp-1",
      optionCount: 2,
      request: HTTP_SPEC,
    }));
    dispatchMocks.dispatchAiActionProposal.mockResolvedValue({ status: "stale" });
    storeState.adapter = {
      getAiActionProposal: vi.fn(async () => heuristic),
      buildLlmDecisionRequest,
      getAiActionProposalFromLlmResponse: vi.fn(),
    };

    const controller = createAIController({
      seats: [{ playerId: 1, difficulty: "Medium", llmSeatIndex: 0 }],
    });
    controller.start();
    // A "stale" dispatch outcome makes the controller re-query the same
    // decision, which is exactly the loop a dead provider would tax forever.
    for (let attempt = 0; attempt < 6; attempt += 1) await runOnce();

    // The provider is tried up to the failure ceiling and then dropped, while
    // the seat keeps acting through the engine AI throughout.
    expect(buildLlmDecisionRequest.mock.calls.length).toBeLessThanOrEqual(3);
    expect(dispatchMocks.dispatchAiActionProposal).toHaveBeenCalledWith(heuristic);
    controller.dispose();
  });

  it("never sends the API key anywhere but the engine's request builder", async () => {
    bindSeatToProvider();
    const buildLlmDecisionRequest = vi.fn<
      (
        difficulty: string,
        playerId: number,
        endpointJson: string,
        historyJson: string,
      ) => Promise<LlmDecisionRequestResult>
    >(async () => ({
      fingerprint: "fp-1",
      optionCount: 2,
      request: HTTP_SPEC,
    }));
    llmMocks.executeLlmRequest.mockResolvedValue('{"choice":0}');
    storeState.adapter = {
      getAiActionProposal: vi.fn(async () => proposal(PASS, "heuristic")),
      buildLlmDecisionRequest,
      getAiActionProposalFromLlmResponse: vi.fn(async () => ({
        proposal: proposal(PASS, "llm-bound"),
      })),
    };

    const controller = createAIController({
      seats: [{ playerId: 1, difficulty: "Medium", llmSeatIndex: 0 }],
    });
    controller.start();
    await runOnce();

    const endpointJson = buildLlmDecisionRequest.mock.calls[0]?.[2] ?? "";
    expect(JSON.parse(endpointJson)).toMatchObject({ apiKey: "k", provider: "OpenAi" });
    // The transport receives only what the engine built — never the profile.
    expect(llmMocks.executeLlmRequest).toHaveBeenCalledWith(HTTP_SPEC, expect.anything());
    controller.dispose();
  });
});
