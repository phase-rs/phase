/**
 * Tests for DraftPodHostAdapter and DraftPodGuestAdapter.
 *
 * Verifies the lifecycle wrapper layer: event mapping, status transitions,
 * and clean delegation to P2PDraftHost/P2PDraftGuest. The underlying
 * PeerJS and WASM layers are mocked — protocol-level tests live in
 * `draftProtocol.test.ts` and `draftPersistence.test.ts`.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DraftPodHostAdapter } from "../draftPodHostAdapter";
import type { DraftPodHostEvent } from "../draftPodHostAdapter";
import { DraftPodGuestAdapter } from "../draftPodGuestAdapter";
import type { DraftPodGuestEvent } from "../draftPodGuestAdapter";
import type { DraftPlayerView } from "../draft-adapter";
import { loadDraftHostSession } from "../../services/draftPersistence";
import type { DraftWorkspaceState } from "../../components/draft/workspace/types";

// ── Mocks ──────────────────────────────────────────────────────────────

// Mock the draft-adapter module — vitest cannot resolve the lazy
// `@wasm/draft` import that DraftAdapter's `ensureDraftWasm` performs.
// The host adapter only uses DraftAdapter to populate the cube CARD_DB;
// the Set-mode tests below never exercise that path, so a no-op mock is
// sufficient.
vi.mock("../draft-adapter", () => ({
  DraftAdapter: vi.fn().mockImplementation(function () {
    return {
      loadCardDatabase: vi.fn(async () => 0),
    };
  }),
}));

// Mock the connection module
vi.mock("../../network/connection", () => ({
  hostRoom: vi.fn(),
  joinRoom: vi.fn(),
}));

// Mock persistence
vi.mock("../../services/draftPersistence", () => ({
  loadDraftHostSession: vi.fn(async () => null),
  loadDraftGuestSession: vi.fn(async () => null),
}));

// Mock P2PDraftHost
const mockHostOnEvent = vi.fn((_handler: (event: Record<string, unknown>) => void) => vi.fn());
const mockHostInitialize = vi.fn(async () => {});
const mockHostStartDraft = vi.fn(async () => {});
const mockHostSubmitHostPick = vi.fn(async () => mockView("Drafting"));
const mockHostSubmitHostPickWithDraftEffect = vi.fn(async () => mockView("Drafting"));
const mockHostSubmitHostDeck = vi.fn(async () => mockView("Deckbuilding"));
const mockHostGetHostView = vi.fn(async () => mockView("Lobby"));
const mockHostKickPlayer = vi.fn();
const mockHostRequestPause = vi.fn();
const mockHostRequestResume = vi.fn();
const mockHostDispose = vi.fn();
const mockHostTerminateDraft = vi.fn(async () => {});
const mockHostRestoreFromPersisted = vi.fn(async (): Promise<DraftPlayerView | null> => null);
const mockHostUpdateWorkspace = vi.fn(async () => {});
const mockHostGetWorkspaceState = vi.fn((): DraftWorkspaceState | null => null);

vi.mock("../p2p-draft-host", () => ({
  P2PDraftHost: vi.fn().mockImplementation(function () {
    return {
      onEvent: mockHostOnEvent,
      initialize: mockHostInitialize,
      startDraft: mockHostStartDraft,
      submitHostPick: mockHostSubmitHostPick,
      submitHostPickWithDraftEffect: mockHostSubmitHostPickWithDraftEffect,
      submitHostDeck: mockHostSubmitHostDeck,
      getHostView: mockHostGetHostView,
      kickPlayer: mockHostKickPlayer,
      requestPause: mockHostRequestPause,
      requestResume: mockHostRequestResume,
      dispose: mockHostDispose,
      terminateDraft: mockHostTerminateDraft,
      restoreFromPersisted: mockHostRestoreFromPersisted,
      updateHostWorkspace: mockHostUpdateWorkspace,
      getHostWorkspaceState: mockHostGetWorkspaceState,
      isFull: false,
      isStarted: false,
      isPaused: false,
    };
  }),
}));

// Mock P2PDraftGuest
const mockGuestOnEvent = vi.fn((_handler: (event: Record<string, unknown>) => void) => vi.fn());
const mockGuestInitialize = vi.fn(async () => {});
const mockGuestSubmitPick = vi.fn(async () => {});
const mockGuestSubmitPickWithDraftEffect = vi.fn(async () => {});
const mockGuestSubmitDeck = vi.fn(async () => {});
const mockGuestUpdateWorkspace = vi.fn(async () => {});
const mockGuestLeave = vi.fn(async () => {});
const mockGuestDispose = vi.fn();
let mockGuestRecoveryRevoked = false;

vi.mock("../p2p-draft-guest", () => ({
  P2PDraftGuest: vi.fn().mockImplementation(function () {
    return {
      onEvent: mockGuestOnEvent,
      initialize: mockGuestInitialize,
      submitPick: mockGuestSubmitPick,
      submitPickWithDraftEffect: mockGuestSubmitPickWithDraftEffect,
      submitDeck: mockGuestSubmitDeck,
      updateWorkspace: mockGuestUpdateWorkspace,
      leave: mockGuestLeave,
      dispose: mockGuestDispose,
      get isRecoveryRevoked() { return mockGuestRecoveryRevoked; },
      view: null,
      seat: null,
      token: null,
    };
  }),
}));

// ── Helpers ────────────────────────────────────────────────────────────

function mockView(status: string): DraftPlayerView {
  return {
    status: status as DraftPlayerView["status"],
    kind: "Premier",
    launch_capability: "None",
    current_pack_number: 1,
    pick_number: 1,
    pass_direction: "Left",
    current_pack: null,
    // Premier (CR 905.1a) with no pending pack.
    required_pick_count: 0,
    pick_selection_mode: "Direct",
    pool: [],
    draft_effects: [],
    pool_groups: {
      color_groups: [],
      type_groups: [],
      cmc_groups: [],
      rarity_groups: [],
      type_filter_options: [],
      color_filter_options: [],
      color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
      workspace_capabilities: { rarity_group_order: null },
      workspace_row_classification: {
        creature_instance_ids: [],
        noncreature_instance_ids: [],
      },
    },
    seats: [],
    cards_per_pack: 14,
    pack_sizes: [14, 14, 14],
    pack_set_codes: ["TST", "TST", "TST"],
    pack_pick_steps: [14, 14, 14],
    pick_steps_per_pack: 14,
    pack_count: 3,
    min_deck_size: 40,
    addable_cards: ["Plains", "Island", "Swamp", "Mountain", "Forest"],
    timer_remaining_ms: null,
    standings: [],
    current_round: 0,
    next_pairing_round: 1,
    tournament_format: "Swiss",
    pod_policy: "Competitive",
    pairings: [],
    match_config: { match_type: "Bo1" },
  };
}

const restoredWorkspace: DraftWorkspaceState = {
  schemaVersion: 1,
  placements: { card: { zone: "deck", row: 0, column: 1, order: 0 } },
  virtualBasics: [],
};

function mockHostResult() {
  return {
    roomCode: "ABCDE",
    peerId: "phase2-ABCDE",
    peer: { destroy: vi.fn() } as unknown,
    onGuestConnected: vi.fn(() => vi.fn()),
    destroy: vi.fn(),
  };
}

function mockJoinResult() {
  return {
    conn: { peer: "phase2-ABCDE" } as unknown,
    peer: { id: "guest-peer-id", destroy: vi.fn() } as unknown,
    closeConn: vi.fn(),
    destroyPeer: vi.fn(),
  };
}

// ── DraftPodHostAdapter Tests ──────────────────────────────────────────

describe("DraftPodHostAdapter", () => {
  let adapter: DraftPodHostAdapter;
  let events: DraftPodHostEvent[];

  beforeEach(async () => {
    vi.clearAllMocks();
    const { hostRoom } = await import("../../network/connection");
    (hostRoom as ReturnType<typeof vi.fn>).mockResolvedValue(mockHostResult());

    adapter = new DraftPodHostAdapter();
    events = [];
    adapter.onEvent((e) => events.push(e));
  });

  afterEach(async () => {
    vi.useRealTimers();
    await adapter.dispose();
    vi.unstubAllGlobals();
  });

  /**
   * `initialize()`'s card-data gate is
   * `config.poolInput.type === "Cube" || config.kind === "CommanderDraft"`, and
   * it performs a REAL `fetch(__CARD_DATA_URL__)` whose `!resp.ok` branch
   * throws. This suite's own header notes its Set-mode tests never exercise
   * that path — a `kind: "CommanderDraft"` row does, so it must stub the fetch
   * or it reds on a network error rather than on its claim.
   */
  function stubCardDataFetch() {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({ ok: true, text: async () => "{}" })),
    );
  }

  it("starts in idle status", () => {
    expect(adapter.status).toBe("idle");
    expect(adapter.roomCode).toBeNull();
  });

  it("transitions to lobby after initialization", async () => {
    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });

    expect(adapter.status).toBe("lobby");
    expect(adapter.roomCode).toBe("ABCDE");

    const statusEvents = events.filter((e) => e.type === "statusChanged");
    expect(statusEvents).toContainEqual({ type: "statusChanged", status: "connecting" });
    expect(statusEvents).toContainEqual({ type: "statusChanged", status: "lobby" });
    expect(events).toContainEqual({ type: "roomCreated", roomCode: "ABCDE" });
  });

  it("passes the configured backup endpoint to the production P2P host", async () => {
    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      backupEndpoint: "https://phase.example",
    });

    const { P2PDraftHost } = await import("../p2p-draft-host");
    expect(P2PDraftHost).toHaveBeenCalledWith(
      expect.anything(),
      expect.any(Function),
      expect.anything(),
      "Premier",
      8,
      "Host",
      "Swiss",
      "Competitive",
      undefined,
      undefined,
      "ABCDE",
      "https://phase.example",
    );
  });

  it("can suspend without terminating the persisted host draft", async () => {
    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });

    await adapter.dispose({ preserveSession: true });

    expect(mockHostDispose).toHaveBeenCalledTimes(1);
    expect(mockHostTerminateDraft).not.toHaveBeenCalled();
  });

  it("emits error on connection failure", async () => {
    const { hostRoom } = await import("../../network/connection");
    (hostRoom as ReturnType<typeof vi.fn>).mockRejectedValue(new Error("signaling down"));

    await expect(
      adapter.initialize({
        poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      }),
    ).rejects.toThrow("signaling down");

    expect(adapter.status).toBe("error");
    expect(events).toContainEqual({ type: "error", message: "signaling down" });
  });

  it("delegates startDraft to P2PDraftHost", async () => {
    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });

    await adapter.startDraft();
    expect(mockHostStartDraft).toHaveBeenCalledOnce();
  });

  it("restores MatchInProgress host sessions without falling back to drafting", async () => {
    vi.mocked(loadDraftHostSession).mockResolvedValue({
      persistenceId: "draft-1",
      roomCode: "ABCDE",
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      seatTokens: { 0: "host" },
      seatNames: { 0: "Host" },
      kickedTokens: [],
      draftStarted: true,
      draftCode: "ABCDE",
      draftSessionJson: "{}",
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    });
    const restoredView = mockView("MatchInProgress");
    mockHostRestoreFromPersisted.mockResolvedValue(restoredView);
    mockHostGetWorkspaceState.mockReturnValue(restoredWorkspace);

    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      persistenceId: "draft-1",
    });

    expect(adapter.status).toBe("matchInProgress");
    expect(events).toContainEqual({
      type: "workspaceRestored",
      workspaceState: restoredWorkspace,
    });
    expect(events).toContainEqual({ type: "viewUpdated", view: restoredView });
    expect(events.findIndex((event) => event.type === "workspaceRestored"))
      .toBeLessThan(events.findIndex((event) => event.type === "viewUpdated"));
  });

  it.each([
    ["non-null", restoredWorkspace],
    ["null", null],
  ] as const)("emits %s restoration before view and host initialization resolves", async (_label, state) => {
    vi.mocked(loadDraftHostSession).mockResolvedValue({
      persistenceId: "draft-1",
      roomCode: "ABCDE",
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      seatTokens: {},
      seatNames: { 0: "Host" },
      kickedTokens: [],
      draftStarted: true,
      draftCode: "ABCDE",
      draftSessionJson: "{}",
      poolInput: { type: "Set", data: { set_pool_json: "{}" } },
      perSeatWorkspaceSnapshots: {},
    });
    const restoredView = mockView("Drafting");
    mockHostRestoreFromPersisted.mockResolvedValue(restoredView);
    mockHostGetWorkspaceState.mockReturnValue(state);
    let resolveInitialize!: () => void;
    mockHostInitialize.mockImplementationOnce(() => new Promise<void>((resolve) => {
      resolveInitialize = resolve;
    }));
    const observed: string[] = [];
    adapter.onEvent((event) => {
      if (event.type === "workspaceRestored") {
        observed.push(`workspace:${event.workspaceState === null ? "null" : "state"}`);
        void expect(adapter.updateWorkspace(restoredWorkspace)).rejects.toThrow("Host not initialized");
      }
      if (event.type === "viewUpdated") observed.push("view");
    });

    const initialization = adapter.initialize({
      poolInput: { type: "Set", data: { set_pool_json: "{}" } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      persistenceId: "draft-1",
    });
    await vi.waitFor(() => expect(observed).toEqual([
      `workspace:${state === null ? "null" : "state"}`,
      "view",
    ]));
    expect(mockHostInitialize).toHaveBeenCalledOnce();
    expect(mockHostUpdateWorkspace).not.toHaveBeenCalled();
    resolveInitialize();
    await initialization;
    await adapter.updateWorkspace(restoredWorkspace);
    expect(mockHostUpdateWorkspace).toHaveBeenCalledWith(restoredWorkspace);
  });

  it("awaits workspace delegation and propagates host rejection", async () => {
    await expect(adapter.updateWorkspace(restoredWorkspace)).rejects.toThrow("Host not initialized");
    await adapter.initialize({
      poolInput: { type: "Set", data: { set_pool_json: "{}" } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });
    mockHostUpdateWorkspace.mockRejectedValueOnce(new Error("host update failed"));
    await expect(adapter.updateWorkspace(restoredWorkspace)).rejects.toThrow("host update failed");
  });

  it("destroys a post-hostRoom host when its restore is aborted", async () => {
    const { hostRoom } = await import("../../network/connection");
    const hostResult = mockHostResult();
    (hostRoom as ReturnType<typeof vi.fn>).mockResolvedValue(hostResult);
    vi.mocked(loadDraftHostSession).mockResolvedValue({
      persistenceId: "draft-1",
      roomCode: "ABCDE",
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      seatTokens: { 0: "host" },
      seatNames: { 0: "Host" },
      kickedTokens: [],
      draftStarted: true,
      draftCode: "draft-1",
      draftSessionJson: "{}",
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    });
    let resolveRestore!: (view: DraftPlayerView | null) => void;
    mockHostRestoreFromPersisted.mockImplementationOnce(() => new Promise((resolve) => {
      resolveRestore = resolve;
    }));
    const controller = new AbortController();
    const initializing = adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      persistenceId: "draft-1",
      signal: controller.signal,
    });

    await Promise.resolve();
    await Promise.resolve();
    controller.abort();
    resolveRestore(mockView("Drafting"));

    await expect(initializing).rejects.toThrow("initialization aborted");
    expect(mockHostDispose).toHaveBeenCalledOnce();
    expect(hostResult.destroy).toHaveBeenCalledOnce();
    expect(mockHostInitialize).not.toHaveBeenCalled();
    expect(adapter.roomCode).toBeNull();
  });

  it("cleans a pending local host when the adapter is disposed during restore", async () => {
    const { hostRoom } = await import("../../network/connection");
    const hostResult = mockHostResult();
    (hostRoom as ReturnType<typeof vi.fn>).mockResolvedValue(hostResult);
    let resolveSession!: (session: null) => void;
    vi.mocked(loadDraftHostSession).mockImplementationOnce(() => new Promise<null>((resolve) => {
      resolveSession = resolve;
    }));
    const initializing = adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      persistenceId: "draft-1",
    });

    await Promise.resolve();
    await Promise.resolve();
    const disposing = adapter.dispose({ preserveSession: true });
    resolveSession(null);

    await disposing;
    await expect(initializing).rejects.toThrow("initialization aborted");
    expect(hostResult.destroy).toHaveBeenCalledOnce();
    expect(mockHostDispose).toHaveBeenCalledOnce();
    expect(mockHostInitialize).not.toHaveBeenCalled();
  });

  it("destroys a late hostRoom result before the same room code is rehosted", async () => {
    const { hostRoom } = await import("../../network/connection");
    const staleHostResult = mockHostResult();
    let resolveHostRoom!: (result: ReturnType<typeof mockHostResult>) => void;
    (hostRoom as ReturnType<typeof vi.fn>).mockImplementationOnce(
      () => new Promise<ReturnType<typeof mockHostResult>>((resolve) => {
        resolveHostRoom = resolve;
      }),
    );
    const initializing = adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      preferredRoomCode: "ABCDE",
    });

    await Promise.resolve();
    const disposing = adapter.dispose({ preserveSession: true });
    resolveHostRoom(staleHostResult);

    await disposing;
    await expect(initializing).rejects.toThrow("initialization aborted");
    expect(staleHostResult.destroy).toHaveBeenCalledOnce();

    const replacement = new DraftPodHostAdapter();
    const replacementResult = mockHostResult();
    (hostRoom as ReturnType<typeof vi.fn>).mockResolvedValueOnce(replacementResult);
    await replacement.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      preferredRoomCode: "ABCDE",
    });

    expect(replacement.roomCode).toBe("ABCDE");
    await replacement.dispose({ preserveSession: true });
  });

  it("does not publish a host if cancellation wins its local initialize race", async () => {
    let resolveInitialize!: () => void;
    mockHostInitialize.mockImplementationOnce(() => new Promise<void>((resolve) => {
      resolveInitialize = resolve;
    }));
    const controller = new AbortController();
    const initializing = adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      signal: controller.signal,
    });

    await Promise.resolve();
    await Promise.resolve();
    controller.abort();
    resolveInitialize();

    await expect(initializing).rejects.toThrow("initialization aborted");
    expect(mockHostDispose).toHaveBeenCalledOnce();
    await expect(adapter.startDraft()).rejects.toThrow("Host not initialized");
  });

  it("delegates submitPick and returns view", async () => {
    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });

    const view = await adapter.submitPick(["card-123"]);
    expect(mockHostSubmitHostPick).toHaveBeenCalledWith(["card-123"]);
    expect(view.status).toBe("Drafting");
  });

  it("delegates draft-effect picks and returns view", async () => {
    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });

    const view = await adapter.submitPickWithDraftEffect("cogwork-1", ["card-1", "card-2"]);
    expect(mockHostSubmitHostPickWithDraftEffect).toHaveBeenCalledWith("cogwork-1", ["card-1", "card-2"]);
    expect(view.status).toBe("Drafting");
  });

  it("delegates submitDeck and returns view", async () => {
    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });

    // The designation is deliberately NOT derivable from the deck: a
    // passthrough that forwarded the deck, or dropped the argument, reds here.
    const view = await adapter.submitDeck(["Plains", "Island"], ["Kenrith, the Returned King"]);
    expect(mockHostSubmitHostDeck).toHaveBeenCalledWith(
      ["Plains", "Island"],
      ["Kenrith, the Returned King"],
    );
    expect(view.status).toBe("Deckbuilding");
  });

  it("delegates host controls (kick, pause, resume)", async () => {
    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });

    adapter.kickPlayer(3, "AFK");
    expect(mockHostKickPlayer).toHaveBeenCalledWith(3, "AFK");

    adapter.requestPause();
    expect(mockHostRequestPause).toHaveBeenCalledOnce();

    adapter.requestResume();
    expect(mockHostRequestResume).toHaveBeenCalledOnce();
  });

  it("throws when actions called before initialize", async () => {
    await expect(adapter.startDraft()).rejects.toThrow("Host not initialized");
    await expect(adapter.submitPick(["x"])).rejects.toThrow("Host not initialized");
    expect(() => adapter.kickPlayer(1)).toThrow("Host not initialized");
  });

  it("maps P2PDraftHost events to DraftPodHostEvents", async () => {
    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });

    // Extract the event handler registered on P2PDraftHost
    const hostEventHandler = mockHostOnEvent.mock.calls[0][0];

    // Simulate host events
    hostEventHandler({ type: "seatJoined", seatIndex: 2, displayName: "Alice" });
    expect(events).toContainEqual({
      type: "seatJoined",
      seatIndex: 2,
      displayName: "Alice",
    });

    const view = mockView("Drafting");
    hostEventHandler({ type: "draftStarted", view });
    expect(events).toContainEqual({ type: "draftStarted", view });
    expect(adapter.status).toBe("drafting");

    hostEventHandler({ type: "draftComplete" });
    expect(adapter.status).toBe("deckbuilding");

    // U21/Shape B: `allDecksSubmitted` no longer writes a status — the engine's
    // own published view does, on the `viewUpdated` that follows it. REPAIRED
    // rather than deleted, so this route still asserts the adapter reaches a
    // status; the value now comes from `hostStatusForView` instead of a literal.
    hostEventHandler({ type: "allDecksSubmitted" });
    hostEventHandler({ type: "viewUpdated", view: mockView("Pairing") });
    expect(adapter.status).toBe("pairing");

    hostEventHandler({
      type: "bo3ChoosePlayDraw",
      matchId: "match-1",
      gameNumber: 2,
      score: { p0_wins: 0, p1_wins: 1, draws: 0 },
      timerMs: 10_000,
    });
    expect(events).toContainEqual({
      type: "bo3ChoosePlayDraw",
      matchId: "match-1",
      gameNumber: 2,
      score: { p0_wins: 0, p1_wins: 1, draws: 0 },
      timerMs: 10_000,
    });
  });

  /**
   * PF2 ROW 3a — the adapter's status comes from the engine-published view,
   * not from the `allDecksSubmitted` event.
   *
   * The test supplies the two EVENTS; production computes the asserted status
   * via `hostStatusForView`, which maps `Complete` -> `"complete"`. Hand-feeding
   * events is sound HERE — this row's subject is the adapter's own mapping —
   * and is NOT sound for the page-level rows, whose subject is that the event
   * is emitted at all.
   *
   * REVERT-PROBE: restore `this.setStatus("pairing");` to
   * `case "allDecksSubmitted":` and drop
   * `this.setStatus(hostStatusForView(event.view));` from `case "viewUpdated":`.
   * The recorded sequence is then `["pairing"]`.
   */
  it("never reports pairing for a Complete pod", async () => {
    stubCardDataFetch();
    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "CommanderDraft",
      podSize: 4,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });
    const hostEventHandler = mockHostOnEvent.mock.calls[0][0];
    const statuses: string[] = [];
    adapter.onEvent((e) => {
      if (e.type === "statusChanged") statuses.push(e.status);
    });

    hostEventHandler({ type: "allDecksSubmitted" });
    hostEventHandler({ type: "viewUpdated", view: mockView("Complete") });

    // Paired positive reach-guard: a handler that was never captured emits
    // nothing, and "does not contain pairing" is vacuously true of [].
    expect(statuses.length).toBeGreaterThan(0);
    // REVERT-FAILING: `["pairing"]` at base.
    expect(statuses).not.toContain("pairing");
    expect(statuses).toContain("complete");
    expect(adapter.status).toBe("complete");
  });

  /**
   * PF2 ROW 3a, second hostile sibling (must stay green). U21 does not touch
   * the round-advance path, so `roundAdvanced` must STILL reach `"pairing"`.
   */
  it("still reaches pairing on roundAdvanced", async () => {
    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });
    const hostEventHandler = mockHostOnEvent.mock.calls[0][0];

    hostEventHandler({ type: "roundAdvanced" });

    expect(adapter.status).toBe("pairing");
  });

  /**
   * PF2 ROW 3a, third hostile sibling (must stay green). U21 does not touch the
   * `pairingsGenerated` arm either.
   */
  it("still reaches matchInProgress on pairingsGenerated", async () => {
    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });
    const hostEventHandler = mockHostOnEvent.mock.calls[0][0];

    hostEventHandler({ type: "pairingsGenerated", round: 1, pairings: [] });

    expect(adapter.status).toBe("matchInProgress");
  });

  it("cleans up on dispose", async () => {
    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });

    await adapter.dispose();
    expect(mockHostTerminateDraft).toHaveBeenCalledOnce();
    expect(adapter.status).toBe("idle");
    expect(adapter.roomCode).toBeNull();
  });

  it("unsubscribes event listener on returned unsub function", async () => {
    const extraEvents: DraftPodHostEvent[] = [];
    const unsub = adapter.onEvent((e) => extraEvents.push(e));

    await adapter.initialize({
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
    });

    unsub();
    // Simulate more events — the unsubscribed listener should not receive them
    const hostEventHandler = mockHostOnEvent.mock.calls[0][0];
    hostEventHandler({ type: "roundComplete" });

    // Only the events[] listener (still active) should get the event;
    // extraEvents should have stopped receiving after unsub()
    const preUnsub = extraEvents.length;
    hostEventHandler({ type: "roundComplete" });
    expect(extraEvents.length).toBe(preUnsub);
  });
});

// ── DraftPodGuestAdapter Tests ─────────────────────────────────────────

describe("DraftPodGuestAdapter", () => {
  let adapter: DraftPodGuestAdapter;
  let events: DraftPodGuestEvent[];

  beforeEach(async () => {
    vi.clearAllMocks();
    mockGuestRecoveryRevoked = false;
    const { joinRoom } = await import("../../network/connection");
    (joinRoom as ReturnType<typeof vi.fn>).mockResolvedValue(mockJoinResult());

    adapter = new DraftPodGuestAdapter();
    events = [];
    adapter.onEvent((e) => events.push(e));
  });

  afterEach(async () => {
    await adapter.dispose();
    vi.useRealTimers();
  });

  it("starts in idle status", () => {
    expect(adapter.status).toBe("idle");
    expect(adapter.seatIndex).toBeNull();
    expect(adapter.draftCode).toBeNull();
    expect(adapter.currentView).toBeNull();
  });

  it("transitions to lobby after initialization", async () => {
    await adapter.initialize({
      kind: "new",
      roomCode: "ABCDE",
      displayName: "Alice",
    });

    expect(adapter.status).toBe("lobby");

    const statusEvents = events.filter((e) => e.type === "statusChanged");
    expect(statusEvents).toContainEqual({ type: "statusChanged", status: "connecting" });
    expect(statusEvents).toContainEqual({ type: "statusChanged", status: "lobby" });
  });

  it("does not look up a reconnect token for a new join", async () => {
    const { loadDraftGuestSession } = await import("../../services/draftPersistence");
    await adapter.initialize({
      kind: "new",
      roomCode: "ABCDE",
      displayName: "Alice",
    });

    expect(loadDraftGuestSession).not.toHaveBeenCalled();
  });

  it("refuses to send a reconnect capability to a different host peer", async () => {
    const { joinRoom } = await import("../../network/connection");
    const mismatched = {
      ...mockJoinResult(),
      conn: { peer: "phase2-OTHER" },
    };
    (joinRoom as ReturnType<typeof vi.fn>).mockResolvedValueOnce(mismatched);

    await expect(adapter.initialize({
      kind: "reconnect",
      roomCode: "ABCDE",
      displayName: "Alice",
      hostPeerId: "phase2-ABCDE",
      draftToken: "opaque-token",
    })).rejects.toThrow("host changed");
    expect(mockGuestInitialize).not.toHaveBeenCalled();
    expect(mismatched.destroyPeer).toHaveBeenCalledOnce();
  });

  it("retries only credentialed reconnect room joins within a bounded budget", async () => {
    vi.useFakeTimers();
    const { joinRoom } = await import("../../network/connection");
    (joinRoom as ReturnType<typeof vi.fn>)
      .mockRejectedValueOnce(new Error("first transport failure"))
      .mockRejectedValueOnce(new Error("second transport failure"))
      .mockResolvedValueOnce(mockJoinResult());

    const reconnecting = adapter.initialize({
      kind: "reconnect",
      roomCode: "ABCDE",
      displayName: "Alice",
      hostPeerId: "phase2-ABCDE",
      draftToken: "opaque-token",
    });
    await vi.runAllTimersAsync();
    await expect(reconnecting).resolves.toBeUndefined();
    expect(joinRoom).toHaveBeenCalledTimes(3);
  });

  it("aborts a credentialed reconnect join without starting another attempt", async () => {
    vi.useFakeTimers();
    const { joinRoom } = await import("../../network/connection");
    (joinRoom as ReturnType<typeof vi.fn>).mockRejectedValue(new Error("transport failure"));
    const controller = new AbortController();

    const reconnecting = adapter.initialize({
      kind: "reconnect",
      roomCode: "ABCDE",
      displayName: "Alice",
      hostPeerId: "phase2-ABCDE",
      draftToken: "opaque-token",
      signal: controller.signal,
    });
    await Promise.resolve();
    controller.abort();
    await expect(reconnecting).rejects.toMatchObject({ name: "AbortError" });
    expect(joinRoom).toHaveBeenCalledTimes(1);
  });

  it("passes a reconnect config through without a join fallback", async () => {
    await adapter.initialize({
      kind: "reconnect",
      roomCode: "ABCDE",
      displayName: "Alice",
      hostPeerId: "phase2-ABCDE",
      draftToken: "opaque-token",
    });

    const { P2PDraftGuest } = await import("../p2p-draft-guest");
    expect(P2PDraftGuest).toHaveBeenLastCalledWith(
      expect.anything(),
      "phase2-ABCDE",
      expect.anything(),
      expect.objectContaining({ kind: "reconnect", draftToken: "opaque-token" }),
    );
  });

  it("preserves recovery credentials for lifecycle disposal but clears them on explicit leave", async () => {
    await adapter.initialize({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    await adapter.dispose();
    expect(mockGuestDispose).toHaveBeenCalled();
    expect(mockGuestLeave).not.toHaveBeenCalled();

    adapter = new DraftPodGuestAdapter();
    await adapter.initialize({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    await adapter.dispose({ preserveRecovery: false });
    expect(mockGuestLeave).toHaveBeenCalled();
  });

  it("retains guest event ownership when an explicit leave is not acknowledged", async () => {
    await adapter.initialize({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const guestEventHandler = mockGuestOnEvent.mock.calls[0][0];
    const guestEventUnsub = mockGuestOnEvent.mock.results[0]!.value as ReturnType<typeof vi.fn>;
    mockGuestLeave.mockRejectedValueOnce(new Error("Draft host disconnected before acknowledging leave"));

    await expect(adapter.dispose({ preserveRecovery: false })).rejects.toThrow("disconnected before acknowledging leave");
    expect(guestEventUnsub).not.toHaveBeenCalled();

    guestEventHandler({ type: "reconnecting", attempt: 1 });
    expect(events).toContainEqual({ type: "reconnecting", attempt: 1 });

    await adapter.dispose({ preserveRecovery: false });
    expect(guestEventUnsub).toHaveBeenCalledOnce();
  });

  it("locally disposes an explicitly exited guest after its recovery is revoked", async () => {
    await adapter.initialize({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const guestEventHandler = mockGuestOnEvent.mock.calls[0][0];
    mockGuestRecoveryRevoked = true;
    guestEventHandler({ type: "hostLeft", reason: "Host left" });

    await expect(adapter.dispose({ preserveRecovery: false })).resolves.toBeUndefined();
    expect(mockGuestDispose).toHaveBeenCalled();
    expect(mockGuestLeave).not.toHaveBeenCalled();
  });

  it("emits error on connection failure", async () => {
    const { joinRoom } = await import("../../network/connection");
    (joinRoom as ReturnType<typeof vi.fn>).mockRejectedValue(
      new Error("Connection timed out"),
    );

    await expect(
      adapter.initialize({ kind: "new", roomCode: "ZZZZZ", displayName: "Bob" }),
    ).rejects.toThrow("Connection timed out");

    expect(adapter.status).toBe("error");
    expect(events).toContainEqual({
      type: "error",
      message: "Connection timed out",
    });
  });

  it("delegates submitPick to P2PDraftGuest", async () => {
    await adapter.initialize({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });

    await adapter.submitPick(["card-456"]);
    expect(mockGuestSubmitPick).toHaveBeenCalledWith(["card-456"]);
  });

  it("delegates draft-effect picks to P2PDraftGuest", async () => {
    await adapter.initialize({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });

    await adapter.submitPickWithDraftEffect("cogwork-1", ["card-1", "card-2"]);
    expect(mockGuestSubmitPickWithDraftEffect).toHaveBeenCalledWith("cogwork-1", ["card-1", "card-2"]);
  });

  it("delegates submitDeck to P2PDraftGuest", async () => {
    await adapter.initialize({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });

    await adapter.submitDeck(["Swamp", "Mountain"], ["Gyruda, Doom of Depths"]);
    expect(mockGuestSubmitDeck).toHaveBeenCalledWith(
      ["Swamp", "Mountain"],
      ["Gyruda, Doom of Depths"],
    );
  });

  it("awaits workspace delegation and propagates guest rejection", async () => {
    await expect(adapter.updateWorkspace(restoredWorkspace)).rejects.toThrow("Guest not initialized");
    await adapter.initialize({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    await adapter.updateWorkspace(restoredWorkspace);
    expect(mockGuestUpdateWorkspace).toHaveBeenCalledWith(restoredWorkspace);
    mockGuestUpdateWorkspace.mockRejectedValueOnce(new Error("guest update failed"));
    await expect(adapter.updateWorkspace(restoredWorkspace)).rejects.toThrow("guest update failed");
  });

  it("throws when actions called before initialize", async () => {
    await expect(adapter.submitPick(["x"])).rejects.toThrow("Guest not initialized");
    await expect(adapter.submitDeck([], [])).rejects.toThrow("Guest not initialized");
  });

  it("maps P2PDraftGuest events to DraftPodGuestEvents", async () => {
    await adapter.initialize({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });

    const guestEventHandler = mockGuestOnEvent.mock.calls[0][0];

    // Simulate join
    guestEventHandler({ type: "joined", seatIndex: 3, draftCode: "draft-001" });
    expect(adapter.seatIndex).toBe(3);
    expect(adapter.draftCode).toBe("draft-001");
    expect(events).toContainEqual({
      type: "joined",
      seatIndex: 3,
      draftCode: "draft-001",
    });

    guestEventHandler({ type: "workspaceRestored", workspaceState: restoredWorkspace });
    guestEventHandler({ type: "viewUpdated", view: mockView("Lobby") });
    guestEventHandler({ type: "workspaceRestored", workspaceState: null });
    expect(events.slice(-3).map((event) => event.type)).toEqual([
      "workspaceRestored",
      "viewUpdated",
      "workspaceRestored",
    ]);
    expect(events[events.length - 1]).toEqual({
      type: "workspaceRestored",
      workspaceState: null,
    });

    // Simulate view update with drafting status
    const draftView = mockView("Drafting");
    guestEventHandler({ type: "viewUpdated", view: draftView });
    expect(adapter.currentView).toBe(draftView);
    expect(adapter.status).toBe("drafting");

    // Simulate pause
    guestEventHandler({ type: "draftPaused", reason: "Player disconnected" });
    expect(events).toContainEqual({
      type: "draftPaused",
      reason: "Player disconnected",
    });

    // Simulate resume
    guestEventHandler({ type: "draftResumed" });
    expect(events).toContainEqual({ type: "draftResumed" });

    guestEventHandler({
      type: "reconnectFailed",
      failure: { kind: "retryable", message: "Host is restarting" },
    });
    expect(events).toContainEqual({
      type: "reconnectFailed",
      failure: { kind: "retryable", message: "Host is restarting" },
    });

    // Simulate kicked
    guestEventHandler({ type: "kicked", reason: "Host kicked you" });
    expect(adapter.status).toBe("kicked");

    // Simulate pairing
    guestEventHandler({
      type: "pairing",
      round: 1,
      table: 2,
      opponentName: "Bob",
      matchHostPeerId: "phase2-XYZ",
      matchId: "match-001",
    });
    expect(events).toContainEqual({
      type: "pairing",
      round: 1,
      table: 2,
      opponentName: "Bob",
      matchHostPeerId: "phase2-XYZ",
      matchId: "match-001",
    });
  });

  it("updates status based on DraftPlayerView status", async () => {
    await adapter.initialize({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const guestEventHandler = mockGuestOnEvent.mock.calls[0][0];

    guestEventHandler({ type: "viewUpdated", view: mockView("Drafting") });
    expect(adapter.status).toBe("drafting");

    guestEventHandler({ type: "viewUpdated", view: mockView("Deckbuilding") });
    expect(adapter.status).toBe("deckbuilding");

    guestEventHandler({ type: "viewUpdated", view: mockView("Complete") });
    expect(adapter.status).toBe("complete");
  });

  it("cleans up on dispose", async () => {
    await adapter.initialize({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });

    await adapter.dispose();
    expect(mockGuestDispose).toHaveBeenCalledOnce();
    expect(mockGuestLeave).not.toHaveBeenCalled();
    expect(adapter.status).toBe("idle");
    expect(adapter.currentView).toBeNull();
    expect(adapter.seatIndex).toBeNull();
  });
});
