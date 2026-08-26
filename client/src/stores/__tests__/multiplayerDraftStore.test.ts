/**
 * Tests for multiplayerDraftStore Zustand store.
 *
 * Verifies store state transitions and action delegation. The underlying
 * adapters are mocked — this layer tests the Zustand projection.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  draftPodScreen,
  useMultiplayerDraftStore,
  type DraftPodScreen,
} from "../multiplayerDraftStore";
import { DraftPodHostAdapter } from "../../adapter/draftPodHostAdapter";
import type { DraftPlayerView } from "../../adapter/draft-adapter";
import type { ActionRejection, EngineAdapter } from "../../adapter/types";
import { actionRejectionError } from "../../adapter/types";
import { DraftPauseReason, type DraftMatchLaunch } from "../../network/draftProtocol";
import {
  commandAcknowledgement,
  draftIntergameDigest,
  type DraftIntergameCommand,
} from "../../services/intergameCommandLedger";
import { useAppNotificationStore } from "../../stores/appToastStore";

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
  submitPickWithDraftEffect: vi.fn(async () => mockView("Drafting")),
  submitDeck: vi.fn(async () => mockView("Deckbuilding")),
  getHostView: vi.fn(async () => mockView("Lobby")),
  kickPlayer: vi.fn(),
  requestPause: vi.fn(),
  requestResume: vi.fn(),
  overrideMatchResult: vi.fn(async () => {}),
  submitMatchSettlement: vi.fn(async () => {}),
  submitAuthorized: vi.fn(),
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
  submitPickWithDraftEffect: vi.fn(async () => {}),
  submitDeck: vi.fn(async () => {}),
  submitAuthorized: vi.fn(),
  acknowledgeAuthorized: vi.fn(),
  dispose: vi.fn(async () => {}),
  status: "idle" as const,
  seatIndex: null,
  draftCode: null,
  currentView: null,
};

vi.mock("../../adapter/draftPodHostAdapter", () => ({
  DraftPodHostAdapter: vi.fn().mockImplementation(function () {
    return { ...mockHostAdapter };
  }),
}));

vi.mock("../../adapter/draftPodGuestAdapter", () => ({
  DraftPodGuestAdapter: vi.fn().mockImplementation(function () {
    return { ...mockGuestAdapter };
  }),
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
    draft_effects: [],
    pool_groups: {
      color_groups: [],
      type_groups: [],
      cmc_groups: [],
      rarity_groups: [],
      type_filter_options: [],
      color_filter_options: [],
      color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
    },
    seats: [],
    cards_per_pack: 14,
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

// ── Tests ──────────────────────────────────────────────────────────────

describe("multiplayerDraftStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    capturedHostEventHandler = null;
    capturedGuestEventHandler = null;
    useMultiplayerDraftStore.getState().reset();
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
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
    it("hands a completed host session off before joining and gates its late events", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });
      const staleHostEvent = capturedHostEventHandler!;

      await useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "Guest" });
      staleHostEvent({ type: "roomCreated", roomCode: "STALE" });

      expect(mockHostAdapter.dispose).toHaveBeenCalledWith({ preserveSession: true });
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", roomCode: null });
    });

    it("waits for a cancelled recovery's same-ID host cleanup before starting its replacement", async () => {
      const config = {
        poolInput: { type: "Set" as const, data: { set_pool_json: "{}" } },
        kind: "Premier" as const,
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss" as const,
        podPolicy: "Competitive" as const,
        persistenceId: "shared-recovery",
      };
      await expect(useMultiplayerDraftStore.getState().hostDraft(config)).resolves.toBe(true);

      let releaseCleanup!: () => void;
      mockHostAdapter.dispose.mockImplementationOnce(() => new Promise<void>((resolve) => {
        releaseCleanup = resolve;
      }));
      const replacement = useMultiplayerDraftStore.getState().hostDraft(config);
      await Promise.resolve();

      expect(vi.mocked(DraftPodHostAdapter)).toHaveBeenCalledTimes(1);
      releaseCleanup();

      await expect(replacement).resolves.toBe(true);
      expect(vi.mocked(DraftPodHostAdapter)).toHaveBeenCalledTimes(2);
    });

    it("disposes a superseded in-flight host after its late initialization resolves", async () => {
      let resolveHost!: () => void;
      mockHostAdapter.initialize.mockImplementationOnce(() => new Promise<void>((resolve) => {
        resolveHost = resolve;
      }));

      const first = useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });
      await useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "Guest" });
      resolveHost();
      await first;

      expect(mockHostAdapter.dispose).toHaveBeenCalledWith({ preserveSession: true });
      expect(useMultiplayerDraftStore.getState().role).toBe("guest");
    });

    it("releases an in-flight host when its owning route aborts", async () => {
      let resolveHost!: () => void;
      mockHostAdapter.initialize.mockImplementationOnce(() => new Promise<void>((resolve) => {
        resolveHost = resolve;
      }));
      const controller = new AbortController();
      const hosting = useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
        signal: controller.signal,
      });

      await Promise.resolve();
      controller.abort();
      resolveHost();

      await expect(hosting).resolves.toBe(false);
      expect(mockHostAdapter.dispose).toHaveBeenCalledWith({ preserveSession: true });
      expect(useMultiplayerDraftStore.getState().role).not.toBe("host");
    });

    it("releases an initialized host when its owning route later aborts", async () => {
      const controller = new AbortController();
      await expect(useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
        signal: controller.signal,
      })).resolves.toBe(true);

      controller.abort();
      await Promise.resolve();

      expect(mockHostAdapter.dispose).toHaveBeenCalledWith({ preserveSession: true });
      expect(useMultiplayerDraftStore.getState().role).not.toBe("host");
    });

    it("sets role to host and phase to connecting", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
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
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
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
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
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
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
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

    it("projects restored MatchInProgress views into match phase", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });

      const view = mockView("MatchInProgress");
      capturedHostEventHandler!({ type: "viewUpdated", view });

      const state = useMultiplayerDraftStore.getState();
      expect(state.phase).toBe("matchInProgress");
      expect(state.view).toBe(view);
    });

    it("pairingsGenerated advances currentRound, leaves nextPairingRound to viewUpdated, and the phase change retires the error", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });

      // The host's real round boundary, in the order the adapter emits it: the
      // round-2 view lands on the round-complete screen, the failed attempt
      // raises the banner, and the retry's `roundAdvanced` opens the pairing
      // window before pairing generation commits round 3.
      const view = {
        ...mockView("RoundComplete"),
        current_round: 2,
        next_pairing_round: 3,
      };
      capturedHostEventHandler!({ type: "viewUpdated", view });
      // Reach-guard: prove the error is live before the boundary, so a null
      // afterwards cannot mean "it was never set".
      capturedHostEventHandler!({
        type: "error",
        message: "Failed to advance round",
      });
      expect(useMultiplayerDraftStore.getState().error).toBe(
        "Failed to advance round",
      );
      // The retry. `roundAdvanced` is what moves the phase off `roundComplete`,
      // and that transition is what retires the banner — one step before
      // `pairingsGenerated`, and for guests as well as the host.
      capturedHostEventHandler!({ type: "roundAdvanced" });
      capturedHostEventHandler!({ type: "pairingsGenerated", round: 3, pairings: [] });

      const state = useMultiplayerDraftStore.getState();
      // Reach-guards: both events were demonstrably delivered, so a wrong
      // `nextPairingRound` cannot be confused with "no event reached the store".
      expect(state.view).toBe(view);
      expect(state.phase).toBe("matchInProgress");
      expect(state.currentRound).toBe(3);
      // `pairingsGenerated` writes only the round it owns. The `3 / 3` relation
      // is the deliberate window, not an accident: anyone later adding an
      // inlined `nextPairingRound: event.round + 1` to that handler — the
      // TypeScript re-derivation this work exists to abolish — yields 4 here.
      expect(state.nextPairingRound).toBe(3);
      // REVERT-FAILING: remove the `clearErrorOnPhaseChange` wrap and this goes
      // red with "Failed to advance round" — the retry that WORKED would still
      // show the failed attempt's banner.
      expect(state.error).toBeNull();
    });

    it("handles host-seat Bo3 prompt messages", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
        kind: "Traditional",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });

      capturedHostEventHandler!({
        type: "bo3ChoosePlayDraw",
        matchId: "match-1",
        gameNumber: 2,
        score: { p0_wins: 0, p1_wins: 1, draws: 0 },
        timerMs: 10_000,
      });

      let state = useMultiplayerDraftStore.getState();
      expect(state.playDrawPrompt).toEqual({
        matchId: "match-1",
        gameNumber: 2,
        score: { p0_wins: 0, p1_wins: 1, draws: 0 },
        timerMs: 10_000,
      });
      expect(state.timerRemainingMs).toBe(10_000);

      capturedHostEventHandler!({
        type: "bo3GameStart",
        matchId: "match-1",
        gameNumber: 2,
        firstPlayerSeat: 0,
      });

      state = useMultiplayerDraftStore.getState();
      expect(state.phase).toBe("matchInProgress");
      expect(state.playDrawPrompt).toBeNull();
      expect(state.sideboardSubmitted).toBe(false);
    });

    it("reports active bot match results back to the pod host", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });

      useMultiplayerDraftStore.setState({
        matchPairing: {
          type: "Bot",
          matchId: "match-1",
          round: 1,
          localSeat: 0,
          botSeat: 4,
          botName: "Chandra",
          deckPayload: {
            player: { main_deck: [], sideboard: [], commander: [] },
            opponent: { main_deck: [], sideboard: [], commander: [] },
            ai_decks: [],
          },
          matchConfig: { match_type: "Bo1" },
          binding: {
            podId: "draft-1", matchId: "match-1", round: 1,
            sessionKey: "session-1", lease: "lease-1", nonce: "nonce-1",
            revision: 0, matchAuthoritySeat: 0,
          },
        },
      });

      await useMultiplayerDraftStore.getState().reportActiveMatchGameResult(1);

      expect(mockHostAdapter.submitMatchSettlement).toHaveBeenCalledWith(expect.objectContaining({
        winnerSeat: 4,
      }));
    });

    it("reports active match concessions as opponent wins", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });

      useMultiplayerDraftStore.setState({
        matchPairing: {
          type: "Bot",
          matchId: "match-2",
          round: 1,
          localSeat: 0,
          botSeat: 5,
          botName: "Jace",
          deckPayload: {
            player: { main_deck: [], sideboard: [], commander: [] },
            opponent: { main_deck: [], sideboard: [], commander: [] },
            ai_decks: [],
          },
          matchConfig: { match_type: "Bo1" },
          binding: {
            podId: "draft-1", matchId: "match-2", round: 1,
            sessionKey: "session-2", lease: "lease-2", nonce: "nonce-2",
            revision: 0, matchAuthoritySeat: 0,
          },
        },
      });

      await useMultiplayerDraftStore.getState().reportActiveMatchConcession();

      expect(mockHostAdapter.submitMatchSettlement).toHaveBeenCalledWith(expect.objectContaining({
        winnerSeat: 5,
      }));
    });
  });

  describe("joinDraft", () => {
    it("sets role to guest and phase to connecting", async () => {
      await useMultiplayerDraftStore.getState().joinDraft({
        kind: "new",
        roomCode: "ABCDE",
        displayName: "Alice",
      });

      const state = useMultiplayerDraftStore.getState();
      expect(state.role).toBe("guest");
    });

    it("releases an in-flight guest recovery when its route aborts", async () => {
      let resolveGuest!: () => void;
      mockGuestAdapter.initialize.mockImplementationOnce(() => new Promise<void>((resolve) => {
        resolveGuest = resolve;
      }));
      const controller = new AbortController();
      const joining = useMultiplayerDraftStore.getState().joinDraft({
        kind: "reconnect",
        roomCode: "ABCDE",
        displayName: "Alice",
        hostPeerId: "phase2-ABCDE",
        draftToken: "opaque-token",
        signal: controller.signal,
      });

      await Promise.resolve();
      controller.abort();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });

      resolveGuest();
      await joining;
      expect(mockGuestAdapter.dispose).toHaveBeenCalledWith({ preserveRecovery: true });
    });

    it("sets seatIndex and draftCode on joined event", async () => {
      await useMultiplayerDraftStore.getState().joinDraft({
        kind: "new",
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
        kind: "new",
        roomCode: "ABCDE",
        displayName: "Alice",
      });

      capturedGuestEventHandler!({
        type: "draftPaused",
        reason: DraftPauseReason.PlayerDisconnected,
      });

      let state = useMultiplayerDraftStore.getState();
      expect(state.paused).toBe(true);
      expect(state.pauseReason).toBe(DraftPauseReason.PlayerDisconnected);

      capturedGuestEventHandler!({ type: "draftResumed" });
      state = useMultiplayerDraftStore.getState();
      expect(state.paused).toBe(false);
      expect(state.pauseReason).toBeNull();
    });

    it("tracks pairing info", async () => {
      await useMultiplayerDraftStore.getState().joinDraft({
        kind: "new",
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
        kind: "new",
        roomCode: "ABCDE",
        displayName: "Alice",
      });

      capturedGuestEventHandler!({ type: "kicked", reason: "AFK" });

      const state = useMultiplayerDraftStore.getState();
      expect(state.phase).toBe("kicked");
      expect(state.error).toBe("AFK");
    });

    it("retains typed reconnect failure semantics for the recovery screen", async () => {
      await useMultiplayerDraftStore.getState().joinDraft({
        kind: "reconnect",
        roomCode: "ABCDE",
        displayName: "Alice",
        hostPeerId: "phase2-ABCDE",
        draftToken: "opaque-token",
      });

      capturedGuestEventHandler!({
        type: "reconnectFailed",
        failure: { kind: "retryable", message: "Host is restarting" },
      });

      expect(useMultiplayerDraftStore.getState()).toMatchObject({
        error: "Host is restarting",
        guestRecoveryFailure: { kind: "retryable", message: "Host is restarting" },
      });
    });

    it("retires a guest error when the phase changes, and only then", async () => {
      await useMultiplayerDraftStore.getState().joinDraft({
        kind: "new",
        roomCode: "ABCDE",
        displayName: "Alice",
      });

      capturedGuestEventHandler!({
        type: "viewUpdated",
        view: mockView("MatchInProgress"),
      });
      capturedGuestEventHandler!({
        type: "error",
        message: "Failed to start match",
      });
      // Reach-guard: the error is live before any of the three steps below, so
      // a null at the end cannot mean "it was never set".
      expect(useMultiplayerDraftStore.getState().error).toBe(
        "Failed to start match",
      );

      // (i) A same-phase broadcast must NOT clear. `viewUpdated` fires on every
      // pick, seat change and timer sync; clearing on the mere presence of a
      // `phase` key would erase an error the user has not read.
      capturedGuestEventHandler!({
        type: "viewUpdated",
        view: mockView("MatchInProgress"),
      });
      expect(useMultiplayerDraftStore.getState().phase).toBe("matchInProgress");
      expect(useMultiplayerDraftStore.getState().error).toBe(
        "Failed to start match",
      );

      // (ii) A payload that names no phase at all must NOT clear.
      capturedGuestEventHandler!({
        type: "lobbyUpdate",
        seats: [],
        joined: 2,
        total: 8,
      });
      expect(useMultiplayerDraftStore.getState().joined).toBe(2);
      expect(useMultiplayerDraftStore.getState().error).toBe(
        "Failed to start match",
      );

      // (iii) A real phase change retires it.
      capturedGuestEventHandler!({
        type: "viewUpdated",
        view: mockView("RoundComplete"),
      });

      const state = useMultiplayerDraftStore.getState();
      // Reach-guard: the clearing event was delivered and applied.
      expect(state.phase).toBe("roundComplete");
      // REVERT-FAILING: remove the `clearErrorOnPhaseChange` wrap and this goes
      // red — a guest error raised in one phase would ride along into the next.
      expect(state.error).toBeNull();
    });

    it("clears a stale message when the guest enters a message-less error phase, but not one that follows the flip", async () => {
      await useMultiplayerDraftStore.getState().joinDraft({
        kind: "new",
        roomCode: "ABCDE",
        displayName: "Alice",
      });

      capturedGuestEventHandler!({ type: "error", message: "gone" });
      // Reach-guard, as above.
      expect(useMultiplayerDraftStore.getState().error).toBe("gone");

      // `reconnectFailed`'s shape: the adapter flips status to `error` and the
      // event that follows carries no message, so nothing replaces the stale
      // string. Attributing an unrelated earlier failure to a failed reconnect
      // is worse than the page's own generic copy — accepted, and pinned here.
      capturedGuestEventHandler!({ type: "statusChanged", status: "error" });

      expect(useMultiplayerDraftStore.getState().phase).toBe("error");
      // REVERT-FAILING: remove the wrap and "gone" survives into the error screen.
      expect(useMultiplayerDraftStore.getState().error).toBeNull();

      // The sibling ordering both `initialize()` catches use — status flip
      // first, message second — must leave the message intact.
      capturedGuestEventHandler!({
        type: "statusChanged",
        status: "matchInProgress",
      });
      capturedGuestEventHandler!({ type: "error", message: "boom" });

      const state = useMultiplayerDraftStore.getState();
      expect(state.phase).toBe("matchInProgress");
      expect(state.error).toBe("boom");
    });
  });

  describe("shared actions", () => {
    it("submits a draft-effect pick through the host adapter", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });

      await useMultiplayerDraftStore.getState().submitPickWithDraftEffect(
        "cogwork-1",
        ["card-1", "card-2"],
      );

      expect(mockHostAdapter.submitPickWithDraftEffect).toHaveBeenCalledWith(
        "cogwork-1",
        ["card-1", "card-2"],
      );
    });

    it("selectCard and confirmPick work together", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
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

    it("autoPickCard submits from the visible pack without manual selection", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });

      useMultiplayerDraftStore.setState({
        view: {
          ...mockView("Drafting"),
          current_pack: [
            {
              instance_id: "card-123",
              name: "Lightning Bolt",
              set_code: "tst",
              collector_number: "1",
              rarity: "common",
              colors: ["R"],
              cmc: 1,
              type_line: "Instant",
            },
          ],
        },
      });

      await useMultiplayerDraftStore.getState().autoPickCard();

      expect(mockHostAdapter.submitPick).toHaveBeenCalledWith("card-123");
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

  describe("authorized intergame actions", () => {
    it("reports structured rejections without leaking either sideboard or play-draw submission", async () => {
      const launch: DraftMatchLaunch = {
        type: "HumanHost",
        matchId: "action-rejection-match",
        matchRoomCode: "MATCH",
        round: 1,
        localSeat: 0,
        opponentSeat: 1,
        opponentName: "Alice",
        matchHostPeerId: "peer-0",
        deckPayload: {
          player: { main_deck: [], sideboard: [], commander: [] },
          opponent: { main_deck: [], sideboard: [], commander: [] },
          ai_decks: [],
        },
        matchConfig: { match_type: "Bo3" },
        binding: {
          podId: "pod-1", matchId: "action-rejection-match", round: 1,
          sessionKey: "session", lease: "lease", nonce: "nonce",
          revision: 1, matchAuthoritySeat: 0,
        },
      };
      const rejection: ActionRejection = {
        code: "invalid_action",
        disposition: "invalid",
        message: "Engine error: ObjectId(199) cannot change deck partitions",
        related_object_ids: [199],
      };
      const adapter = {
        submitAction: vi.fn()
          .mockRejectedValueOnce(actionRejectionError(rejection))
          .mockRejectedValueOnce(actionRejectionError({
            code: "stale_action",
            disposition: "stale",
            message: "This action is no longer current",
            related_object_ids: [199],
          })),
      } as unknown as EngineAdapter;
      useMultiplayerDraftStore.setState({
        role: "host",
        seatIndex: 0,
        matchPairing: launch,
        matchAdapter: adapter,
      });
      const command = (
        commandId: string,
        payload: DraftIntergameCommand["payload"],
      ): DraftIntergameCommand => ({
        commandId,
        matchId: launch.matchId,
        gameNumber: 2,
        seat: 0,
        payload,
        launchPayload: launch,
        launchDigest: draftIntergameDigest(launch),
        payloadDigest: draftIntergameDigest(payload),
        status: "Authorized",
      });
      const sideboard = command("sideboard-rejection", {
        type: "SubmitSideboard", main: [], sideboard: [],
      });

      await expect(useMultiplayerDraftStore.getState().submitAuthorized(
        sideboard,
        commandAcknowledgement(sideboard),
      )).resolves.toBeUndefined();

      expect(adapter.submitAction).toHaveBeenCalledWith({
        type: "SubmitSideboard",
        data: { main: [], sideboard: [] },
      }, 0);
      expect(useAppNotificationStore.getState().notification).toEqual({
        title: "Action failed",
        description: rejection.message,
      });

      useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
      const playDraw = command("play-draw-stale", { type: "ChoosePlayDraw", playFirst: true });

      await expect(useMultiplayerDraftStore.getState().submitAuthorized(
        playDraw,
        commandAcknowledgement(playDraw),
      )).resolves.toBeUndefined();

      expect(adapter.submitAction).toHaveBeenLastCalledWith({
        type: "ChoosePlayDraw",
        data: { play_first: true },
      }, 0);
      expect(useAppNotificationStore.getState().notification).toBeNull();
    });
  });

  describe("leave", () => {
    it("resets state to initial", async () => {
      await useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
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

  // ── Bo3 intergame overlay lifetime ───────────────────────────────────
  //
  // The overlay is no longer a `MultiplayerDraftPhase` member. `draftPodScreen`
  // derives it from two live store fields, so a status writer cannot clobber it
  // (conjunct 1 stays true) and a prompt clear releases it (conjunct 2).
  //
  // Production precondition, reproduced by every fixture below: the phase is
  // ALREADY `matchInProgress` when a prompt arrives — the host writes it in
  // `startMatch` before the match adapter that emits the prompt exists, and the
  // guest writes it on `matchStart`. That is why the three enter sites write no
  // `phase` at all.
  describe("intergame overlay lifetime", () => {
    const SIDEBOARD_PROMPT = {
      type: "bo3SideboardPrompt",
      matchId: "bo3-1",
      gameNumber: 2,
      score: { p0_wins: 1, p1_wins: 0, draws: 0 },
      loserSeat: 1,
      timerMs: 60_000,
    };

    async function hostInMatch(): Promise<void> {
      await useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { set_pool_json: "{}" } },
        kind: "Traditional",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
      });
      capturedHostEventHandler!({ type: "statusChanged", status: "matchInProgress" });
    }

    async function guestInMatch(): Promise<void> {
      await useMultiplayerDraftStore.getState().joinDraft({
        kind: "new",
        roomCode: "ABCDE",
        displayName: "Alice",
      });
      capturedGuestEventHandler!({ type: "statusChanged", status: "matchInProgress" });
    }

    async function enterOverlay(role: "host" | "guest"): Promise<void> {
      if (role === "host") {
        await hostInMatch();
        capturedHostEventHandler!(SIDEBOARD_PROMPT);
      } else {
        await guestInMatch();
        capturedGuestEventHandler!(SIDEBOARD_PROMPT);
      }
    }

    // S1 — the five status writers cannot clobber the overlay (host arms).
    it("holds the overlay across every host status write", async () => {
      await enterOverlay("host");

      // Reach-guard: the fixture really is in the intergame window.
      expect(useMultiplayerDraftStore.getState().sideboardPrompt).not.toBeNull();
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");

      // The nesting that licenses keying the screen on `sideboardPrompt` alone:
      // `bo3ChoosePlayDraw` sets `playDrawPrompt` and leaves `sideboardPrompt`
      // set, so a `playDrawPrompt` disjunct would be unreachable.
      capturedHostEventHandler!({
        type: "bo3ChoosePlayDraw",
        matchId: "bo3-1",
        gameNumber: 2,
        score: { p0_wins: 1, p1_wins: 0, draws: 0 },
        timerMs: 10_000,
      });
      expect(useMultiplayerDraftStore.getState().sideboardPrompt).not.toBeNull();
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");

      // The rule these rows pin — "a status write cannot clobber the overlay" —
      // is enforced by `tsc`, not by a runtime line, so restoring the BASE
      // clobber leaves S1 and S2 GREEN: the assertion is on the SCREEN, which
      // `draftPodScreen` re-derives from the still-live `sideboardPrompt`
      // regardless of what the clobber did to `phase`. (Measured.)
      // REVERT-FAILING, unique to S1+S2: add `sideboardPrompt: null` to both
      // `bo3ChoosePlayDraw` arms — that breaks the nesting invariant asserted
      // just above, which is what licenses keying the screen on one field.
      capturedHostEventHandler!({ type: "viewUpdated", view: mockView("MatchInProgress") });
      expect(useMultiplayerDraftStore.getState().phase).toBe("matchInProgress");
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");

      capturedHostEventHandler!({ type: "statusChanged", status: "matchInProgress" });
      expect(useMultiplayerDraftStore.getState().phase).toBe("matchInProgress");
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");

      capturedHostEventHandler!({ type: "draftStarted", view: mockView("MatchInProgress") });
      expect(useMultiplayerDraftStore.getState().phase).toBe("matchInProgress");
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");
    });

    // S2 — the same statement on the guest handler, which is a physically
    // separate switch a per-site guard would have had to be remembered in twice.
    // Shares S1's killer (see the annotation there); the guest `bo3ChoosePlayDraw`
    // arm is the half of it this row covers.
    it("holds the overlay across every guest status write", async () => {
      await enterOverlay("guest");

      expect(useMultiplayerDraftStore.getState().sideboardPrompt).not.toBeNull();
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");

      capturedGuestEventHandler!({
        type: "bo3ChoosePlayDraw",
        matchId: "bo3-1",
        gameNumber: 2,
        score: { p0_wins: 1, p1_wins: 0, draws: 0 },
        timerMs: 10_000,
      });
      expect(useMultiplayerDraftStore.getState().sideboardPrompt).not.toBeNull();
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");

      capturedGuestEventHandler!({ type: "viewUpdated", view: mockView("MatchInProgress") });
      expect(useMultiplayerDraftStore.getState().phase).toBe("matchInProgress");
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");

      capturedGuestEventHandler!({ type: "statusChanged", status: "matchInProgress" });
      expect(useMultiplayerDraftStore.getState().phase).toBe("matchInProgress");
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");
    });

    // S3 — the multi-authority hostile fixture: a live prompt (authority 2 says
    // "overlay") against a session that has moved on (authority 1 says
    // "tournament over" / "kicked" / "host left" / "abandoned"). Conjunct 1 wins,
    // and the prompt is asserted STILL set so the release is attributable to it.
    it.each<[string, "host" | "guest", () => void, DraftPodScreen]>([
      [
        "host viewUpdated(Complete)",
        "host",
        () => capturedHostEventHandler!({ type: "viewUpdated", view: mockView("Complete") }),
        "complete",
      ],
      [
        "host viewUpdated(Abandoned)",
        "host",
        () => capturedHostEventHandler!({ type: "viewUpdated", view: mockView("Abandoned") }),
        "error",
      ],
      [
        "guest kicked",
        "guest",
        () => capturedGuestEventHandler!({ type: "kicked", reason: "AFK" }),
        "kicked",
      ],
      [
        "guest hostLeft",
        "guest",
        () => capturedGuestEventHandler!({ type: "hostLeft", reason: "Host left the draft" }),
        "hostLeft",
      ],
    ])("releases the overlay when the pod session moves on: %s", async (_name, role, deliver, expected) => {
      await enterOverlay(role);
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");

      deliver();

      const state = useMultiplayerDraftStore.getState();
      // REVERT-FAILING: delete `state.phase === "matchInProgress" &&` from
      // `draftPodScreen` and this row answers `"betweenGames"` instead.
      expect(state.sideboardPrompt).not.toBeNull();
      expect(draftPodScreen(state)).toBe(expected);
    });

    // S4 — conjunct 2: the next Bo3 game releases the overlay, and does so
    // WITHOUT a phase move (asserted), so the release is the prompt clear.
    it.each<[string, "host" | "guest", () => void]>([
      [
        "host bo3GameStarted",
        "host",
        () => capturedHostEventHandler!({ type: "bo3GameStarted", matchId: "bo3-1", gameNumber: 2 }),
      ],
      [
        "host bo3GameStart",
        "host",
        () =>
          capturedHostEventHandler!({
            type: "bo3GameStart",
            matchId: "bo3-1",
            gameNumber: 2,
            firstPlayerSeat: 0,
          }),
      ],
      [
        "guest bo3GameStart",
        "guest",
        () =>
          capturedGuestEventHandler!({
            type: "bo3GameStart",
            matchId: "bo3-1",
            gameNumber: 2,
            firstPlayerSeat: 0,
          }),
      ],
    ])("releases the overlay when the next Bo3 game starts: %s", async (_name, role, deliver) => {
      await enterOverlay(role);
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");

      deliver();

      const state = useMultiplayerDraftStore.getState();
      // REVERT-FAILING: drop this arm's `sideboardPrompt: null` write and the
      // screen stays `"betweenGames"` forever.
      expect(state.sideboardPrompt).toBeNull();
      expect(state.playDrawPrompt).toBeNull();
      expect(state.phase).toBe("matchInProgress");
      expect(draftPodScreen(state)).toBe("matchInProgress");
    });

    // S5 — the pre-existing `roundComplete` stranding is released for free.
    // `disposeMatchAdapter` clears both prompts and writes no `phase`, so at BASE
    // the overlay's phase survived its own data.
    it("releases the overlay when roundComplete disposes the match adapter", async () => {
      await enterOverlay("host");
      useMultiplayerDraftStore.setState({ matchAdapter: { dispose() {} } });

      // Reach-guard: `disposeMatchAdapter`'s clearing `set` is inside
      // `if (state.matchAdapter)`, so without an adapter this row would measure
      // the early return instead.
      expect(useMultiplayerDraftStore.getState().matchAdapter).not.toBeNull();
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");

      capturedHostEventHandler!({ type: "roundComplete" });

      const state = useMultiplayerDraftStore.getState();
      // REVERT-FAILING: drop `sideboardPrompt: null` from `disposeMatchAdapter`.
      expect(state.sideboardPrompt).toBeNull();
      expect(state.playDrawPrompt).toBeNull();
      expect(state.phase).toBe("matchInProgress");
      expect(draftPodScreen(state)).toBe("matchInProgress");
    });

    // S6 — error lifetime at the overlay ENTER. Entering is not a phase
    // transition any more, so an unread error survives it; a real boundary still
    // retires it.
    it("keeps an unread error across the overlay enter and retires it at the real boundary", async () => {
      await hostInMatch();
      capturedHostEventHandler!({ type: "error", message: "boom" });

      // Reach-guard: the error is live and the phase is settled BEFORE the enter.
      expect(useMultiplayerDraftStore.getState().error).toBe("boom");
      expect(useMultiplayerDraftStore.getState().phase).toBe("matchInProgress");

      capturedHostEventHandler!(SIDEBOARD_PROMPT);

      // (i) REVERT-FAILING: re-add `phase: "betweenGames"` at the host enter site
      // and this clears — the enter was counted as a phase change at BASE.
      expect(useMultiplayerDraftStore.getState().error).toBe("boom");
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");

      // (ii) The paired positive: `clearErrorOnPhaseChange` still fires at a real
      // phase boundary. Note `sideboardPrompt` is still set here, so this row also
      // reddens if conjunct 1 is deleted from `draftPodScreen`.
      capturedHostEventHandler!({ type: "viewUpdated", view: mockView("RoundComplete") });
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("roundComplete");
      expect(useMultiplayerDraftStore.getState().error).toBeNull();
    });

    // S7 — accepted delta, pinned: the Bo3 game BOUNDARY no longer retires an
    // unread error, because `bo3GameStarted` writes a phase equal to the current
    // one. At BASE this was a `betweenGames → matchInProgress` transition.
    it("keeps an unread error across the Bo3 game boundary", async () => {
      await hostInMatch();
      capturedHostEventHandler!({ type: "error", message: "boom" });
      capturedHostEventHandler!(SIDEBOARD_PROMPT);

      // Reach-guards.
      expect(useMultiplayerDraftStore.getState().error).toBe("boom");
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");

      capturedHostEventHandler!({ type: "bo3GameStarted", matchId: "bo3-1", gameNumber: 2 });

      // PINNING: an equal-phase write must not retire an unread error. Drop the
      // `next.phase === state.phase` disjunct in `clearErrorOnPhaseChange` and
      // this goes red.
      expect(useMultiplayerDraftStore.getState().error).toBe("boom");
      expect(useMultiplayerDraftStore.getState().phase).toBe("matchInProgress");
    });

    // S8 — the same proposition at the entry point #7705's doc block names: a
    // `viewUpdated` broadcast DURING the window (a seat dropping mid-window is
    // enough). At BASE the broadcast flipped the phase off `betweenGames` and
    // took the banner with it.
    it("keeps an unread error across a status broadcast during the window", async () => {
      await hostInMatch();
      capturedHostEventHandler!(SIDEBOARD_PROMPT);
      capturedHostEventHandler!({ type: "error", message: "boom" });

      // Reach-guards.
      expect(useMultiplayerDraftStore.getState().error).toBe("boom");
      expect(draftPodScreen(useMultiplayerDraftStore.getState())).toBe("betweenGames");

      capturedHostEventHandler!({ type: "viewUpdated", view: mockView("MatchInProgress") });

      const state = useMultiplayerDraftStore.getState();
      // PINNING, and the overlay is asserted intact so the row cannot pass by
      // having destroyed it.
      expect(state.error).toBe("boom");
      expect(state.phase).toBe("matchInProgress");
      expect(draftPodScreen(state)).toBe("betweenGames");
    });

    // S9 — pins the safety property that a dismissable overlay depends on:
    // dismissing it routes the page back to the in-match view, whose
    // `startMatch` button is therefore reachable mid-window.
    //
    // Nothing at that call site guards it. The property is structural, in the
    // store: `matchAdapter` is written to `null` in exactly two places —
    // `disposeMatchAdapter`, whose single `set()` also nulls `matchPairing`,
    // `sideboardPrompt` and `playDrawPrompt`, and `initialState`, where the
    // prompts are null too. So a live prompt implies a live adapter, and
    // `startMatch`'s `if (matchAdapter) return gameId` short-circuits ahead of
    // every branch that could build a second adapter or send a start request.
    // The one state with `matchPairing` live and `matchAdapter` null is the
    // window before the adapter is first built, where no prompt exists.
    //
    // Two reviewers have now read this seam as a live hazard, so the
    // short-circuit is load-bearing documentation as much as code — pin it.
    it("starts no second match while a match adapter is live", async () => {
      await enterOverlay("host");
      const matchPairing: DraftMatchLaunch = {
        type: "HumanHost",
        matchId: "bo3-1",
        matchRoomCode: "MATCH",
        round: 1,
        localSeat: 0,
        opponentSeat: 1,
        opponentName: "Alice",
        matchHostPeerId: "peer-0",
        deckPayload: {
          player: { main_deck: [], sideboard: [], commander: [] },
          opponent: { main_deck: [], sideboard: [], commander: [] },
          ai_decks: [],
        },
        matchConfig: { match_type: "Bo3" },
        binding: {
          podId: "pod-1",
          matchId: "bo3-1",
          round: 1,
          sessionKey: "session",
          lease: "lease",
          nonce: "nonce",
          revision: 1,
          matchAuthoritySeat: 0,
        },
      };
      const adapter = { dispose() {} };
      useMultiplayerDraftStore.setState({ matchPairing, matchAdapter: adapter });

      // Reach-guards. `startMatch` returns `null` on `!matchPairing` one line
      // above the short-circuit, so without the first of these the row could
      // pass while measuring that early return instead; without the second it
      // would measure adapter construction.
      const before = useMultiplayerDraftStore.getState();
      expect(before.matchPairing).toBe(matchPairing);
      expect(before.matchAdapter).toBe(adapter);
      expect(before.sideboardPrompt).not.toBeNull();

      const gameId = await useMultiplayerDraftStore.getState().startMatch();

      const state = useMultiplayerDraftStore.getState();
      // REVERT-FAILING (measured): delete `if (matchAdapter) return gameId;`
      // from `startMatch` and this is the only red row in the whole client
      // suite — control falls into the HumanHost arm, tries to stand up a
      // second P2P host, and `startMatch` returns `null` out of its catch
      // instead of the id of the match already in progress.
      expect(gameId).toBe("draft-match-bo3-1");
      expect(state.matchAdapter).toBe(adapter);
      expect(state.sideboardPrompt).toBe(before.sideboardPrompt);
      expect(state.playDrawPrompt).toBeNull();
      expect(state.error).toBeNull();
    });
  });
});
