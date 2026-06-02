/**
 * Shape-level test for DraftPodHostAdapter cube-mode initialization.
 *
 * The discriminating runtime gate for `create_multiplayer_draft` lives in
 * the Rust unit test `create_multiplayer_draft_tests` (crates/draft-wasm).
 * This test verifies the host-side plumbing: when poolInput.type === "Cube",
 * initialize() fetches __CARD_DATA_URL__ and calls
 * DraftAdapter.loadCardDatabase before instantiating P2PDraftHost; when
 * poolInput.type === "Set", the CARD_DB fetch path is skipped.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DraftPodHostAdapter } from "../draftPodHostAdapter";
import type { DraftPodHostEvent } from "../draftPodHostAdapter";

// ── Mocks ──────────────────────────────────────────────────────────────

const mockLoadCardDatabase = vi.fn(async () => 0);

vi.mock("../draft-adapter", () => ({
  DraftAdapter: vi.fn().mockImplementation(() => ({
    loadCardDatabase: mockLoadCardDatabase,
  })),
}));

vi.mock("../../network/connection", () => ({
  hostRoom: vi.fn(async () => ({
    roomCode: "ABCDE",
    peerId: "phase2-ABCDE",
    peer: { destroy: vi.fn() } as unknown,
    onGuestConnected: vi.fn(() => vi.fn()),
    destroy: vi.fn(),
  })),
}));

vi.mock("../../services/draftPersistence", () => ({
  loadDraftHostSession: vi.fn(async () => null),
}));

vi.mock("../p2p-draft-host", () => ({
  P2PDraftHost: vi.fn().mockImplementation(() => ({
    onEvent: vi.fn(() => vi.fn()),
    initialize: vi.fn(async () => {}),
    dispose: vi.fn(),
    terminateDraft: vi.fn(async () => {}),
  })),
}));

const originalFetch = globalThis.fetch;

beforeEach(() => {
  vi.clearAllMocks();
  globalThis.fetch = vi.fn(async () =>
    new Response("{}", { status: 200, headers: { "Content-Type": "application/json" } }),
  );
});

afterEach(() => {
  globalThis.fetch = originalFetch;
});

// ── Tests ──────────────────────────────────────────────────────────────

describe("DraftPodHostAdapter cube-mode initialize", () => {
  let adapter: DraftPodHostAdapter;
  let events: DraftPodHostEvent[];

  beforeEach(() => {
    adapter = new DraftPodHostAdapter();
    events = [];
    adapter.onEvent((e) => events.push(e));
  });

  afterEach(async () => {
    await adapter.dispose();
  });

  it("populates CARD_DB via DraftAdapter.loadCardDatabase for Cube pods", async () => {
    await adapter.initialize({
      poolInput: {
        type: "Cube",
        data: {
          cube_list_text: "1 Lightning Bolt\n",
          cube_name: "Test Cube",
          cube_draft_settings: {
            pod_size: 2,
            pack_count: 1,
            cards_per_pack: 2,
            min_deck_size: 4,
            addable_cards: { policy: "StandardBasics", custom: [] },
          },
        },
      },
      kind: "Premier",
      podSize: 2,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });

    expect(globalThis.fetch).toHaveBeenCalledOnce();
    expect(mockLoadCardDatabase).toHaveBeenCalledOnce();
    expect(adapter.status).toBe("lobby");
  });

  it("skips the CARD_DB fetch for Set pods", async () => {
    await adapter.initialize({
      poolInput: { type: "Set", data: { set_pool_json: "{}" } },
      kind: "Premier",
      podSize: 2,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });

    expect(globalThis.fetch).not.toHaveBeenCalled();
    expect(mockLoadCardDatabase).not.toHaveBeenCalled();
    expect(adapter.status).toBe("lobby");
  });
});
