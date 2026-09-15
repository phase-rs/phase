import { beforeEach, describe, expect, it, vi } from "vitest";

const llmMocks = vi.hoisted(() => ({
  executeLlmRequest: vi.fn<() => Promise<string>>(),
}));

vi.mock("../llmClient", () => ({ executeLlmRequest: llmMocks.executeLlmRequest }));
vi.mock("../../setCatalog", () => ({
  ensureSetCatalog: async () => ({
    mrd: { name: "Mirrodin", released_at: "2003-10-02" },
  }),
}));
vi.mock("../../../game/debugLog", () => ({ debugLog: vi.fn() }));

import type { DraftPlayerView } from "../../../adapter/draft-adapter";
import { botSeatIndices, submitPickWithLlmBots } from "../draftLlm";
import type { LlmProfile } from "../types";

const PROFILE: LlmProfile = {
  id: "p1",
  name: "Test",
  provider: "Anthropic",
  baseUrl: null,
  apiKey: "k",
  model: "claude-sonnet-5",
  maxOutputTokens: null,
  temperature: null,
  enabled: true,
};

const HUMAN_VIEW = { pool: [] } as unknown as DraftPlayerView;
const BOT_VIEW = { pool: [] } as unknown as DraftPlayerView;

interface PickRequestRow {
  seat: number;
  fingerprint: string;
  optionCount: number;
  requiredPickCount: number;
  request: { url: string; method: string; headers: []; body: string };
}

type BuildRequests = (
  endpointJson: string,
  seats: number[],
  setNames: Record<string, string>,
) => PickRequestRow[];

type SubmitWithLlm = (
  cardInstanceId: string,
  responses: { seat: number; fingerprint: string; provider: string; body: string }[],
) => { view: DraftPlayerView; llmOutcomes: { seat: number; used: boolean }[] };

function lease(overrides: Record<string, unknown> = {}) {
  return {
    submitPick: vi.fn((_cardInstanceId: string) => HUMAN_VIEW),
    buildLlmDraftPickRequests: vi.fn<BuildRequests>(() => [
      {
        seat: 1,
        fingerprint: "fp-1",
        optionCount: 15,
        requiredPickCount: 1,
        request: { url: "https://x.test", method: "POST", headers: [], body: "{}" },
      },
    ]),
    submitPickWithLlmBotPicks: vi.fn<SubmitWithLlm>(() => ({
      view: BOT_VIEW,
      llmOutcomes: [{ seat: 1, used: true }],
    })),
    ...overrides,
  };
}

beforeEach(() => {
  llmMocks.executeLlmRequest.mockReset();
});

describe("LLM drafters", () => {
  it("takes the ordinary path when the pod has no bot seats", async () => {
    const l = lease();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const view = await submitPickWithLlmBots(l as any, "card-1", PROFILE, []);

    expect(view).toBe(HUMAN_VIEW);
    expect(l.submitPick).toHaveBeenCalledWith("card-1");
    expect(l.buildLlmDraftPickRequests).not.toHaveBeenCalled();
  });

  it("passes each seat's reply back to the engine to resolve", async () => {
    llmMocks.executeLlmRequest.mockResolvedValue('{"choice":3}');
    const l = lease();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const view = await submitPickWithLlmBots(l as any, "card-1", PROFILE, [1]);

    expect(view).toBe(BOT_VIEW);
    expect(l.submitPickWithLlmBotPicks).toHaveBeenCalledWith("card-1", [
      { seat: 1, fingerprint: "fp-1", provider: "Anthropic", body: '{"choice":3}' },
    ]);
    expect(l.submitPick).not.toHaveBeenCalled();
  });

  it("gives the engine the format's set names so the brief can read 'Triple Mirrodin'", async () => {
    llmMocks.executeLlmRequest.mockResolvedValue('{"choice":0}');
    const l = lease();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    await submitPickWithLlmBots(l as any, "card-1", PROFILE, [1]);

    expect(l.buildLlmDraftPickRequests.mock.calls[0]?.[2]).toEqual({ MRD: "Mirrodin" });
  });

  it("falls back to the engine bots when every provider call fails", async () => {
    llmMocks.executeLlmRequest.mockRejectedValue(new Error("network down"));
    const l = lease();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const view = await submitPickWithLlmBots(l as any, "card-1", PROFILE, [1]);

    expect(view).toBe(HUMAN_VIEW);
    expect(l.submitPick).toHaveBeenCalledWith("card-1");
    expect(l.submitPickWithLlmBotPicks).not.toHaveBeenCalled();
  });

  it("falls back to the engine bots when the engine builds no requests", async () => {
    const l = lease({ buildLlmDraftPickRequests: vi.fn<BuildRequests>(() => []) });
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const view = await submitPickWithLlmBots(l as any, "card-1", PROFILE, [1]);

    expect(view).toBe(HUMAN_VIEW);
    expect(l.submitPick).toHaveBeenCalledWith("card-1");
  });

  it("falls back to the engine bots when request building throws", async () => {
    const l = lease({
      buildLlmDraftPickRequests: vi.fn<BuildRequests>(() => {
        throw new Error("draft session not initialized");
      }),
    });
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const view = await submitPickWithLlmBots(l as any, "card-1", PROFILE, [1]);

    expect(view).toBe(HUMAN_VIEW);
    expect(l.submitPick).toHaveBeenCalledWith("card-1");
  });

  it("still resolves the pick when only some seats answered", async () => {
    llmMocks.executeLlmRequest
      .mockResolvedValueOnce('{"choice":1}')
      .mockRejectedValueOnce(new Error("timeout"));
    const l = lease({
      buildLlmDraftPickRequests: vi.fn<BuildRequests>(() => [
        {
          seat: 1,
          fingerprint: "fp-1",
          optionCount: 15,
          requiredPickCount: 1,
          request: { url: "https://x.test", method: "POST", headers: [], body: "{}" },
        },
        {
          seat: 2,
          fingerprint: "fp-2",
          optionCount: 15,
          requiredPickCount: 1,
          request: { url: "https://x.test", method: "POST", headers: [], body: "{}" },
        },
      ]),
    });
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const view = await submitPickWithLlmBots(l as any, "card-1", PROFILE, [1, 2]);

    expect(view).toBe(BOT_VIEW);
    const responses = l.submitPickWithLlmBotPicks.mock.calls[0]?.[1] ?? [];
    expect(responses).toHaveLength(1);
    expect(responses[0]?.seat).toBe(1);
  });

  it("reads bot seats off the engine-published seat list", () => {
    const view = {
      seats: [
        { seat_index: 0, is_bot: false },
        { seat_index: 1, is_bot: true },
        { seat_index: 2, is_bot: true },
      ],
    } as unknown as DraftPlayerView;

    expect(botSeatIndices(view)).toEqual([1, 2]);
  });
});
