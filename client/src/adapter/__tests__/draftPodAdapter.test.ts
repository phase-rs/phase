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

// ── Mocks ──────────────────────────────────────────────────────────────

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
const mockHostOnEvent = vi.fn(() => vi.fn());
const mockHostInitialize = vi.fn(async () => {});
const mockHostStartDraft = vi.fn(async () => {});
const mockHostSubmitHostPick = vi.fn(async () => mockView("Drafting"));
const mockHostSubmitHostDeck = vi.fn(async () => mockView("Deckbuilding"));
const mockHostGetHostView = vi.fn(async () => mockView("Lobby"));
const mockHostKickPlayer = vi.fn();
const mockHostRequestPause = vi.fn();
const mockHostRequestResume = vi.fn();
const mockHostTerminateDraft = vi.fn(async () => {});
const mockHostRestoreFromPersisted = vi.fn(async () => null);

vi.mock("../p2p-draft-host", () => ({
  P2PDraftHost: vi.fn().mockImplementation(() => ({
    onEvent: mockHostOnEvent,
    initialize: mockHostInitialize,
    startDraft: mockHostStartDraft,
    submitHostPick: mockHostSubmitHostPick,
    submitHostDeck: mockHostSubmitHostDeck,
    getHostView: mockHostGetHostView,
    kickPlayer: mockHostKickPlayer,
    requestPause: mockHostRequestPause,
    requestResume: mockHostRequestResume,
    terminateDraft: mockHostTerminateDraft,
    restoreFromPersisted: mockHostRestoreFromPersisted,
    isFull: false,
    isStarted: false,
    isPaused: false,
  })),
}));

// Mock P2PDraftGuest
const mockGuestOnEvent = vi.fn(() => vi.fn());
const mockGuestInitialize = vi.fn(async () => {});
const mockGuestSubmitPick = vi.fn(async () => {});
const mockGuestSubmitDeck = vi.fn(async () => {});
const mockGuestLeave = vi.fn(async () => {});

vi.mock("../p2p-draft-guest", () => ({
  P2PDraftGuest: vi.fn().mockImplementation(() => ({
    onEvent: mockGuestOnEvent,
    initialize: mockGuestInitialize,
    submitPick: mockGuestSubmitPick,
    submitDeck: mockGuestSubmitDeck,
    leave: mockGuestLeave,
    view: null,
    seat: null,
    token: null,
  })),
}));

// ── Helpers ────────────────────────────────────────────────────────────

function mockView(status: string): DraftPlayerView {
  return {
    status: status as DraftPlayerView["status"],
    kind: "Premier",
    current_pack_number: 1,
    pick_number: 1,
    pass_direction: "Left",
    current_pack: null,
    pool: [],
    seats: [],
    cards_per_pack: 14,
    pack_count: 3,
  };
}

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
    await adapter.dispose();
  });

  it("starts in idle status", () => {
    expect(adapter.status).toBe("idle");
    expect(adapter.roomCode).toBeNull();
  });

  it("transitions to lobby after initialization", async () => {
    await adapter.initialize({
      setPoolJson: "{}",
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
    });

    expect(adapter.status).toBe("lobby");
    expect(adapter.roomCode).toBe("ABCDE");

    const statusEvents = events.filter((e) => e.type === "statusChanged");
    expect(statusEvents).toContainEqual({ type: "statusChanged", status: "connecting" });
    expect(statusEvents).toContainEqual({ type: "statusChanged", status: "lobby" });
    expect(events).toContainEqual({ type: "roomCreated", roomCode: "ABCDE" });
  });

  it("emits error on connection failure", async () => {
    const { hostRoom } = await import("../../network/connection");
    (hostRoom as ReturnType<typeof vi.fn>).mockRejectedValue(new Error("signaling down"));

    await expect(
      adapter.initialize({
        setPoolJson: "{}",
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
      }),
    ).rejects.toThrow("signaling down");

    expect(adapter.status).toBe("error");
    expect(events).toContainEqual({ type: "error", message: "signaling down" });
  });

  it("delegates startDraft to P2PDraftHost", async () => {
    await adapter.initialize({
      setPoolJson: "{}",
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
    });

    await adapter.startDraft();
    expect(mockHostStartDraft).toHaveBeenCalledOnce();
  });

  it("delegates submitPick and returns view", async () => {
    await adapter.initialize({
      setPoolJson: "{}",
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
    });

    const view = await adapter.submitPick("card-123");
    expect(mockHostSubmitHostPick).toHaveBeenCalledWith("card-123");
    expect(view.status).toBe("Drafting");
  });

  it("delegates submitDeck and returns view", async () => {
    await adapter.initialize({
      setPoolJson: "{}",
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
    });

    const view = await adapter.submitDeck(["Plains", "Island"]);
    expect(mockHostSubmitHostDeck).toHaveBeenCalledWith(["Plains", "Island"]);
    expect(view.status).toBe("Deckbuilding");
  });

  it("delegates host controls (kick, pause, resume)", async () => {
    await adapter.initialize({
      setPoolJson: "{}",
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
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
    await expect(adapter.submitPick("x")).rejects.toThrow("Host not initialized");
    expect(() => adapter.kickPlayer(1)).toThrow("Host not initialized");
  });

  it("maps P2PDraftHost events to DraftPodHostEvents", async () => {
    await adapter.initialize({
      setPoolJson: "{}",
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
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

    hostEventHandler({ type: "allDecksSubmitted" });
    expect(adapter.status).toBe("pairing");
  });

  it("cleans up on dispose", async () => {
    await adapter.initialize({
      setPoolJson: "{}",
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
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
      setPoolJson: "{}",
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
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
    const { joinRoom } = await import("../../network/connection");
    (joinRoom as ReturnType<typeof vi.fn>).mockResolvedValue(mockJoinResult());

    adapter = new DraftPodGuestAdapter();
    events = [];
    adapter.onEvent((e) => events.push(e));
  });

  afterEach(async () => {
    await adapter.dispose();
  });

  it("starts in idle status", () => {
    expect(adapter.status).toBe("idle");
    expect(adapter.seatIndex).toBeNull();
    expect(adapter.draftCode).toBeNull();
    expect(adapter.currentView).toBeNull();
  });

  it("transitions to lobby after initialization", async () => {
    await adapter.initialize({
      roomCode: "ABCDE",
      displayName: "Alice",
    });

    expect(adapter.status).toBe("lobby");

    const statusEvents = events.filter((e) => e.type === "statusChanged");
    expect(statusEvents).toContainEqual({ type: "statusChanged", status: "connecting" });
    expect(statusEvents).toContainEqual({ type: "statusChanged", status: "lobby" });
  });

  it("emits error on connection failure", async () => {
    const { joinRoom } = await import("../../network/connection");
    (joinRoom as ReturnType<typeof vi.fn>).mockRejectedValue(
      new Error("Connection timed out"),
    );

    await expect(
      adapter.initialize({ roomCode: "ZZZZZ", displayName: "Bob" }),
    ).rejects.toThrow("Connection timed out");

    expect(adapter.status).toBe("error");
    expect(events).toContainEqual({
      type: "error",
      message: "Connection timed out",
    });
  });

  it("delegates submitPick to P2PDraftGuest", async () => {
    await adapter.initialize({ roomCode: "ABCDE", displayName: "Alice" });

    await adapter.submitPick("card-456");
    expect(mockGuestSubmitPick).toHaveBeenCalledWith("card-456");
  });

  it("delegates submitDeck to P2PDraftGuest", async () => {
    await adapter.initialize({ roomCode: "ABCDE", displayName: "Alice" });

    await adapter.submitDeck(["Swamp", "Mountain"]);
    expect(mockGuestSubmitDeck).toHaveBeenCalledWith(["Swamp", "Mountain"]);
  });

  it("throws when actions called before initialize", async () => {
    await expect(adapter.submitPick("x")).rejects.toThrow("Guest not initialized");
    await expect(adapter.submitDeck([])).rejects.toThrow("Guest not initialized");
  });

  it("maps P2PDraftGuest events to DraftPodGuestEvents", async () => {
    await adapter.initialize({ roomCode: "ABCDE", displayName: "Alice" });

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
    await adapter.initialize({ roomCode: "ABCDE", displayName: "Alice" });
    const guestEventHandler = mockGuestOnEvent.mock.calls[0][0];

    guestEventHandler({ type: "viewUpdated", view: mockView("Drafting") });
    expect(adapter.status).toBe("drafting");

    guestEventHandler({ type: "viewUpdated", view: mockView("Deckbuilding") });
    expect(adapter.status).toBe("deckbuilding");

    guestEventHandler({ type: "viewUpdated", view: mockView("Complete") });
    expect(adapter.status).toBe("complete");
  });

  it("cleans up on dispose", async () => {
    await adapter.initialize({ roomCode: "ABCDE", displayName: "Alice" });

    await adapter.dispose();
    expect(mockGuestLeave).toHaveBeenCalledOnce();
    expect(adapter.status).toBe("idle");
    expect(adapter.currentView).toBeNull();
    expect(adapter.seatIndex).toBeNull();
  });
});
