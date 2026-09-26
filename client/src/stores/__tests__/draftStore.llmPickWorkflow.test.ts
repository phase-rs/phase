// A persisted zustand store captures its storage when this file's imports are
// evaluated, so the working-localStorage install has to precede them.
import "../../test/helpers/persistedStorage";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DraftCardInstance, DraftPlayerView } from "../../adapter/draft-adapter";
import type { DraftRunState } from "../../services/quickDraftPersistence";

/**
 * End-to-end pick workflow for an LLM-driven draft, exercised through the real
 * store action rather than by calling the breaker helpers directly.
 *
 * What this pins that a unit test cannot: that a status-bearing rejected
 * response actually reaches the engine, that the engine's per-seat verdict is
 * what drives the breaker, and that the pick still completes through the
 * heuristic bots. The wiring between those three lives in `draftStore`, so it
 * is the thing under test.
 */
const wasm = vi.hoisted(() => ({
  default: vi.fn(async () => undefined),
  start_quick_draft: vi.fn(),
  start_sealed_draft: vi.fn(),
  start_quick_cube_draft: vi.fn(),
  import_draft_session: vi.fn(),
  load_card_database: vi.fn(() => 0),
  submit_pick: vi.fn(),
  submit_pick_with_draft_effect: vi.fn(),
  auto_pick: vi.fn(),
  submit_deck: vi.fn(),
  suggest_deck: vi.fn(),
  suggest_lands: vi.fn(),
  get_bot_deck: vi.fn(),
  export_draft_session: vi.fn(() => "session"),
  booster_pack_pool_for_game: vi.fn<() => string[] | null | undefined>(() => null),
  buildLlmDraftPickRequests: vi.fn(),
  submitPickWithLlmBotPicks: vi.fn(),
}));

const persistence = vi.hoisted(() => ({
  cleanupQuickDraftLifecycle: vi.fn(async () => undefined),
  drainQuickDraftPersistence: vi.fn(async () => undefined),
  inspectActiveQuickDraftLifecycle: vi.fn<() => Promise<unknown>>(async () => null),
  loadDraftRun: vi.fn<() => Promise<unknown>>(async () => null),
  saveDraftRun: vi.fn(async (_id: string, _run: DraftRunState) => undefined),
  loadQuickDraftSession: vi.fn<() => Promise<unknown>>(async () => null),
  persistQuickDraftSnapshot: vi.fn(async () => undefined),
  scheduleQuickDraftPersistence: vi.fn(),
  cancelQuickDraftPersistence: vi.fn(),
  clearActiveQuickDraft: vi.fn(),
  readActiveQuickDraft: vi.fn<() => ActiveMeta | null>(() => null),
  writeActiveQuickDraft: vi.fn(),
}));
type ActiveMeta = { draftId: string } | null;

const transport = vi.hoisted(() => ({
  executeLlmRequest: vi.fn<
    (spec: unknown, options?: unknown) => Promise<{ status: number; body: string }>
  >(),
}));

vi.mock("@wasm/draft", () => wasm);
vi.mock("../../services/quickDraftPersistence", () => persistence);
vi.mock("../../services/llm/llmClient", () => ({ executeLlmRequest: transport.executeLlmRequest }));
vi.mock("../../services/setCatalog", () => ({ ensureSetCatalog: async () => ({}) }));
vi.mock("../../game/debugLog", () => ({ debugLog: vi.fn() }));

import { isLlmDraftDisabled, resetLlmDraftBreaker } from "../../services/llm/draftLlm";
import { useDraftStore } from "../draftStore";
import { useLlmStore } from "../llmStore";

function card(instanceId: string): DraftCardInstance {
  return {
    instance_id: instanceId,
    name: instanceId,
    set_code: "TST",
    collector_number: instanceId,
    rarity: "common",
    colors: [],
    cmc: 1,
    type_line: "Card",
  };
}

function view(pool: DraftCardInstance[] = []): DraftPlayerView {
  return {
    status: "Drafting",
    kind: "Quick",
    launch_capability: "None",
    distribution: "PickAndPass",
    commanders_required: 0,
    pool,
    current_pack: [],
    draft_effects: [],
    pool_groups: {
      color_groups: [], type_groups: [], cmc_groups: [], rarity_groups: [],
      type_filter_options: [], color_filter_options: [],
      color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
      workspace_capabilities: { rarity_group_order: null },
      workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
    },
    seats: [],
    current_pack_number: 1,
    pick_number: 1,
    pass_direction: "Left",
    cards_per_pack: 14,
    required_pick_count: 0,
    pick_selection_mode: "Direct",
    pick_steps_per_pack: 14,
    pack_count: 3,
    min_deck_size: 40,
    addable_cards: [],
    timer_remaining_ms: null,
    current_round: 0,
    next_pairing_round: 1,
    tournament_format: "Swiss",
    pod_policy: "Casual",
    pairings: [],
    match_config: { match_type: "Bo1" },
  } as unknown as DraftPlayerView;
}

const PROFILE_ID = "llm-draft";

/** The engine-authored pick request for a bot seat. */
const PICK_REQUEST = {
  seat: 1,
  fingerprint: "fp-1",
  optionCount: 14,
  requiredPickCount: 1,
  request: { url: "https://provider.test/v1/chat/completions", method: "POST", headers: [], body: "{}" },
};

async function startDraft(): Promise<void> {
  wasm.start_quick_draft.mockReturnValue(view([]));
  await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
}

beforeEach(async () => {
  vi.clearAllMocks();
  persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue(null);
  useDraftStore.getState().reset();
  resetLlmDraftBreaker();

  useLlmStore.setState({ profiles: [], seatBindings: {}, draftEnabled: false, draftProfileId: null });
  useLlmStore.getState().addProfile({
    id: PROFILE_ID,
    name: "Drafter",
    model: "gpt-5",
    apiKey: "sk-test",
    enabled: true,
  } as never);
  useLlmStore.setState({ draftEnabled: true, draftProfileId: PROFILE_ID });

  wasm.buildLlmDraftPickRequests.mockReturnValue([PICK_REQUEST]);
  await startDraft();
});

afterEach(() => {
  resetLlmDraftBreaker();
  vi.restoreAllMocks();
});

describe("LLM draft pick workflow", () => {
  it("drives the engine, the breaker and the fallback from a rejected status-bearing response", async () => {
    // The provider answers, but with a 401 — bytes that are NOT a usable pick.
    transport.executeLlmRequest.mockResolvedValue({
      status: 401,
      body: JSON.stringify({ error: { message: "Incorrect API key provided" } }),
    });
    // The engine refuses that seat and drafts it with the heuristic bot.
    wasm.submitPickWithLlmBotPicks.mockReturnValue({
      view: view([card("picked")]),
      llmOutcomes: [{ seat: 1, used: false, error: "HTTP 401: Incorrect API key provided" }],
    });

    await useDraftStore.getState().pickCard("picked");

    // 1. The response reached the ENGINE, carrying its status.
    const [, responsesJson] = wasm.submitPickWithLlmBotPicks.mock.calls[0] as [string, string];
    const responses = JSON.parse(responsesJson) as { seat: number; status: number }[];
    expect(responses[0]).toMatchObject({ seat: 1, status: 401 });

    // 2. The pick still completed — the player is never blocked by a bad key.
    expect(useDraftStore.getState().view?.pool.map((c) => c.instance_id)).toEqual(["picked"]);

    // 3. The engine's refusal — not the arrival of bytes — drove the breaker.
    expect(isLlmDraftDisabled(PROFILE_ID)).toBe(false);
  });

  it("trips the breaker after repeated engine refusals and then stops calling the provider", async () => {
    transport.executeLlmRequest.mockResolvedValue({
      status: 429,
      body: JSON.stringify({ error: { message: "rate limited" } }),
    });
    wasm.submitPickWithLlmBotPicks.mockImplementation(() => ({
      view: view([card("picked")]),
      llmOutcomes: [{ seat: 1, used: false, error: "HTTP 429: rate limited" }],
    }));

    for (let round = 0; round < 3; round += 1) {
      wasm.submit_pick.mockReturnValue(view([card("picked")]));
      await useDraftStore.getState().pickCard(`pick-${round}`);
    }

    expect(isLlmDraftDisabled(PROFILE_ID)).toBe(true);

    // Once given up on, the provider is not called again and the pick goes
    // through the ordinary engine-bot path.
    const callsBefore = transport.executeLlmRequest.mock.calls.length;
    wasm.submit_pick.mockReturnValue(view([card("picked"), card("after")]));
    await useDraftStore.getState().pickCard("after");

    expect(transport.executeLlmRequest.mock.calls.length).toBe(callsBefore);
    expect(wasm.submit_pick).toHaveBeenCalledWith("after");
  });

  it("keeps the profile healthy when the engine uses the pick", async () => {
    transport.executeLlmRequest.mockResolvedValue({
      status: 200,
      body: JSON.stringify({ choices: [{ message: { content: '{"choice": 0}' } }] }),
    });
    wasm.submitPickWithLlmBotPicks.mockReturnValue({
      view: view([card("picked")]),
      llmOutcomes: [{ seat: 1, used: true }],
    });

    await useDraftStore.getState().pickCard("picked");

    expect(isLlmDraftDisabled(PROFILE_ID)).toBe(false);
    expect(wasm.submitPickWithLlmBotPicks).toHaveBeenCalledTimes(1);
    // The ordinary path is not also taken.
    expect(wasm.submit_pick).not.toHaveBeenCalled();
  });

  it("never asks the client for a seat list — the engine names its own bot seats", async () => {
    transport.executeLlmRequest.mockResolvedValue({
      status: 200,
      body: JSON.stringify({ choices: [{ message: { content: '{"choice": 0}' } }] }),
    });
    wasm.submitPickWithLlmBotPicks.mockReturnValue({
      view: view([card("picked")]),
      llmOutcomes: [{ seat: 1, used: true }],
    });

    await useDraftStore.getState().pickCard("picked");

    // Endpoint config and set names only: no seat indices cross the boundary.
    expect(wasm.buildLlmDraftPickRequests).toHaveBeenCalledTimes(1);
    expect(wasm.buildLlmDraftPickRequests.mock.calls[0]).toHaveLength(2);
  });
});
