/**
 * Shape-level test for DraftPodHostAdapter cube-mode initialization.
 *
 * The discriminating runtime gate for `create_multiplayer_draft` lives in
 * the Rust unit test `create_multiplayer_draft_tests` (crates/draft-wasm).
 * This test verifies the host-side plumbing: when poolInput.type === "Cube",
 * initialize() fetches __CARD_DATA_URL__ and calls
 * DraftAdapter.loadCardDatabase before instantiating P2PDraftHost; when
 * poolInput.type === "Set", the CARD_DB fetch path is skipped for the four
 * CR 905.1a kinds — but NOT for a CommanderDraft pod, whose bot seats need the
 * database to designate a commander (CR 903.3) and to judge that deck's colour
 * identity (CR 903.5c).
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DraftPodHostAdapter } from "../draftPodHostAdapter";
import type { DraftPodHostEvent } from "../draftPodHostAdapter";
import type { DraftWorkspaceState } from "../../components/draft/workspace/types";

// ── Mocks ──────────────────────────────────────────────────────────────

const mockLoadCardDatabase = vi.fn(async () => 0);
const mockHostInitialize = vi.fn(async () => {});
const mockHostUpdateWorkspace = vi.fn(async (_state: DraftWorkspaceState) => {});

vi.mock("../draft-adapter", () => ({
  DraftAdapter: vi.fn().mockImplementation(function () {
    return {
      loadCardDatabase: mockLoadCardDatabase,
    };
  }),
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
  P2PDraftHost: vi.fn().mockImplementation(function () {
    return {
      onEvent: vi.fn(() => vi.fn()),
      initialize: mockHostInitialize,
      updateHostWorkspace: mockHostUpdateWorkspace,
      getHostWorkspaceState: vi.fn(() => null),
      dispose: vi.fn(),
      terminateDraft: vi.fn(async () => {}),
    };
  }),
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
    expect(mockLoadCardDatabase.mock.invocationCallOrder[0])
      .toBeLessThan(mockHostInitialize.mock.invocationCallOrder[0]);
    expect(adapter.status).toBe("lobby");
  });

  it("delegates Cube workspace updates after database load and host initialization", async () => {
    const state: DraftWorkspaceState = {
      schemaVersion: 1,
      placements: {
        "cube-z": { zone: "sideboard", row: 0, column: 2, order: 0 },
        "cube-a": { zone: "deck", row: 1, column: 0, order: 1 },
      },
      virtualBasics: [],
    };
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

    await adapter.updateWorkspace(state);

    expect(mockLoadCardDatabase.mock.invocationCallOrder[0])
      .toBeLessThan(mockHostInitialize.mock.invocationCallOrder[0]);
    expect(mockHostInitialize.mock.invocationCallOrder[0])
      .toBeLessThan(mockHostUpdateWorkspace.mock.invocationCallOrder[0]);
    expect(mockHostUpdateWorkspace).toHaveBeenCalledWith(state);
    expect(Object.keys(mockHostUpdateWorkspace.mock.calls[0][0].placements)).toEqual(["cube-z", "cube-a"]);
  });

  it("skips the CARD_DB fetch for Set pods", async () => {
    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
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
