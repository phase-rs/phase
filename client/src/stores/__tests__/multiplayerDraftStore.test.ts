/**
 * Tests for multiplayerDraftStore Zustand store.
 *
 * Verifies store state transitions and action delegation. The underlying
 * adapters are mocked — this layer tests the Zustand projection.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useMultiplayerDraftStore } from "../multiplayerDraftStore";
import type { DraftPlayerView } from "../../adapter/draft-adapter";

// ── Mocks ──────────────────────────────────────────────────────────────

let capturedHostEventHandler: ((event: unknown) => void) | null = null;
let capturedGuestEventHandler: ((event: unknown) => void) | null = null;

const mockHostAdapter = {
  onEvent: vi.fn((handler: (event: unknown) => void) => {
    capturedHostEventHandler = handler;
    return vi.fn();
  }),
  initialize: vi.fn(async () => {}),
  startDraft: vi.fn(async () => {}),
  submitPick: vi.fn(async () => mockView("Drafting")),
  submitDeck: vi.fn(async () => mockView("Deckbuilding")),
  getHostView: vi.fn(async () => mockView("Lobby")),
  kickPlayer: vi.fn(),
  requestPause: vi.fn(),
  requestResume: vi.fn(),
  dispose: vi.fn(async () => {}),
  status: "idle" as const,
  roomCode: null,
  isFull: false,
  isStarted: false,
  isPaused: false,
};

const mockGuestAdapter = {
  onEvent: vi.fn((handler: (event: unknown) => void) => {
    capturedGuestEventHandler = handler;
    return vi.fn();
  }),
  initialize: vi.fn(async () => {}),
  submitPick: vi.fn(async () => {}),
  submitDeck: vi.fn(async () => {}),
  dispose: vi.fn(async () => {}),
  status: "idle" as const,
  seatIndex: null,
  draftCode: null,
  currentView: null,
};

vi.mock("../../adapter/draftPodHostAdapter", () => ({
  DraftPodHostAdapter: vi.fn().mockImplementation(() => ({ ...mockHostAdapter })),
}));

vi.mock("../../adapter/draftPodGuestAdapter", () => ({
  DraftPodGuestAdapter: vi.fn().mockImplementation(() => ({ ...mockGuestAdapter })),
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
    timer_remaining_ms: null,
    standings: [],
    current_round: 0,
    tournament_format: "Swiss",
    pod_policy: "Competitive",
    pairings: [],
  };
}

// ── Tests ──────────────────────────────────────────────────────────────

describe("multiplayerDraftStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    capturedHostEventHandler = null;
    capturedGuestEventHandler = null;
    useMultiplayerDraftStore.getState().reset();
  });

  afterEach(async () => {
    await useMultiplayerDraftStore.getState().leave();
  });

  describe("initial state", () => {
    it("starts with idle phase and null role", () => {
      const state = useMultiplayerDraftStore.getState();
      expect(state.phase).toBe("idle");
      expect(state.role).toBeNull();
      expect(state.roomCode).toBeNull();
      expect(state.view).toBeNull();
      expect(state.seats).toEqual([]);
    });
  });

  describe("hostDraft", () => {
    it("sets role to host and phase to connecting", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        setPoolJson: "{}",
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });

      const state = useMultiplayerDraftStore.getState();
      expect(state.role).toBe("host");
      expect(state.seatIndex).toBe(0);
    });

    it("updates roomCode on roomCreated event", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        setPoolJson: "{}",
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });

      // Simulate roomCreated event
      capturedHostEventHandler!({ type: "roomCreated", roomCode: "XYZAB" });
      expect(useMultiplayerDraftStore.getState().roomCode).toBe("XYZAB");
    });

    it("updates view on draftStarted event", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        setPoolJson: "{}",
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });

      const view = mockView("Drafting");
      capturedHostEventHandler!({ type: "draftStarted", view });

      const state = useMultiplayerDraftStore.getState();
      expect(state.view).toBe(view);
      expect(state.phase).toBe("drafting");
    });

    it("tracks lobby state from lobbyUpdate events", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        setPoolJson: "{}",
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });

      capturedHostEventHandler!({
        type: "lobbyUpdate",
        seats: [{ seat_index: 0, display_name: "Host", is_bot: false, connected: true, has_submitted_deck: false }],
        joined: 3,
        total: 8,
      });

      const state = useMultiplayerDraftStore.getState();
      expect(state.joined).toBe(3);
      expect(state.total).toBe(8);
      expect(state.seats).toHaveLength(1);
    });
  });

  describe("joinDraft", () => {
    it("sets role to guest and phase to connecting", async () => {
      await useMultiplayerDraftStore.getState().joinDraft({
        roomCode: "ABCDE",
        displayName: "Alice",
      });

      const state = useMultiplayerDraftStore.getState();
      expect(state.role).toBe("guest");
    });

    it("sets seatIndex and draftCode on joined event", async () => {
      await useMultiplayerDraftStore.getState().joinDraft({
        roomCode: "ABCDE",
        displayName: "Alice",
      });

      capturedGuestEventHandler!({
        type: "joined",
        seatIndex: 4,
        draftCode: "draft-abc",
      });

      const state = useMultiplayerDraftStore.getState();
      expect(state.seatIndex).toBe(4);
      expect(state.draftCode).toBe("draft-abc");
      expect(state.phase).toBe("lobby");
    });

    it("tracks pause state", async () => {
      await useMultiplayerDraftStore.getState().joinDraft({
        roomCode: "ABCDE",
        displayName: "Alice",
      });

      capturedGuestEventHandler!({
        type: "draftPaused",
        reason: "Player disconnected",
      });

      let state = useMultiplayerDraftStore.getState();
      expect(state.paused).toBe(true);
      expect(state.pauseReason).toBe("Player disconnected");

      capturedGuestEventHandler!({ type: "draftResumed" });
      state = useMultiplayerDraftStore.getState();
      expect(state.paused).toBe(false);
      expect(state.pauseReason).toBeNull();
    });

    it("tracks pairing info", async () => {
      await useMultiplayerDraftStore.getState().joinDraft({
        roomCode: "ABCDE",
        displayName: "Alice",
      });

      capturedGuestEventHandler!({
        type: "pairing",
        round: 1,
        table: 2,
        opponentName: "Bob",
        matchHostPeerId: "phase2-XYZ",
        matchId: "match-001",
      });

      const state = useMultiplayerDraftStore.getState();
      expect(state.pairing).toEqual({
        round: 1,
        table: 2,
        opponentName: "Bob",
        matchHostPeerId: "phase2-XYZ",
        matchId: "match-001",
      });
    });

    it("sets phase to kicked on kicked event", async () => {
      await useMultiplayerDraftStore.getState().joinDraft({
        roomCode: "ABCDE",
        displayName: "Alice",
      });

      capturedGuestEventHandler!({ type: "kicked", reason: "AFK" });

      const state = useMultiplayerDraftStore.getState();
      expect(state.phase).toBe("kicked");
      expect(state.error).toBe("AFK");
    });
  });

  describe("shared actions", () => {
    it("selectCard and confirmPick work together", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        setPoolJson: "{}",
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });

      useMultiplayerDraftStore.getState().selectCard("card-123");
      expect(useMultiplayerDraftStore.getState().selectedCard).toBe("card-123");

      await useMultiplayerDraftStore.getState().confirmPick();
      expect(useMultiplayerDraftStore.getState().selectedCard).toBeNull();
    });

    it("addToDeck and removeFromDeck manage mainDeck", () => {
      const { addToDeck, removeFromDeck } = useMultiplayerDraftStore.getState();

      addToDeck("Lightning Bolt");
      addToDeck("Mountain");
      addToDeck("Lightning Bolt");

      expect(useMultiplayerDraftStore.getState().mainDeck).toEqual([
        "Lightning Bolt",
        "Mountain",
        "Lightning Bolt",
      ]);

      removeFromDeck("Lightning Bolt");
      expect(useMultiplayerDraftStore.getState().mainDeck).toEqual([
        "Mountain",
        "Lightning Bolt",
      ]);
    });

    it("setLandCount clamps to zero", () => {
      useMultiplayerDraftStore.getState().setLandCount("Plains", 5);
      expect(useMultiplayerDraftStore.getState().landCounts).toEqual({ Plains: 5 });

      useMultiplayerDraftStore.getState().setLandCount("Plains", -2);
      expect(useMultiplayerDraftStore.getState().landCounts).toEqual({ Plains: 0 });
    });
  });

  describe("leave", () => {
    it("resets state to initial", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        setPoolJson: "{}",
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });

      capturedHostEventHandler!({ type: "roomCreated", roomCode: "XYZAB" });

      await useMultiplayerDraftStore.getState().leave();

      const state = useMultiplayerDraftStore.getState();
      expect(state.role).toBeNull();
      expect(state.phase).toBe("idle");
      expect(state.roomCode).toBeNull();
    });
  });
});
