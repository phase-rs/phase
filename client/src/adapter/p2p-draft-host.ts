/**
 * P2P Draft Tournament Host.
 *
 * Runs the authoritative DraftSession via draft-wasm and coordinates
 * an 8-player draft pod over PeerJS DataChannels. Follows the same
 * hub-and-spoke topology as `P2PHostAdapter` (game host), but speaks
 * the `DraftP2PMessage` protocol instead of `P2PMessage`.
 *
 * Requirements: P2P-01, P2P-03, P2P-05, P2P-06, P2P-07.
 */

import type Peer from "peerjs";
import type { DataConnection } from "peerjs";

import { DraftAdapter } from "./draft-adapter";
import type { DraftPlayerView, MultiplayerSeatDescriptor, PairingView, PoolInput, SeatPublicView } from "./draft-adapter";
import type { PodPolicy, TournamentFormat } from "./draft-adapter";
import {
  createDraftPeerSession,
  type DraftPeerSession,
} from "../network/draftPeerSession";
import { DRAFT_PROTOCOL_VERSION, DraftPauseReason } from "../network/draftProtocol";
import type { DraftDeckPayload, DraftMatchDeckPayload, DraftMatchLaunch, DraftP2PMessage } from "../network/draftProtocol";
import type { MatchConfig, MatchScore } from "./types";
import {
  saveDraftHostSession,
  clearDraftHostSession,
  type PersistedDraftHostSession,
} from "../services/draftPersistence";
import { assignAvatarForSeat } from "../services/playerAvatars";

// ── Types ──────────────────────────────────────────────────────────────

/** Tracks Bo3 match state between games for a single pairing. */
interface Bo3MatchState {
  seatA: number;
  seatB: number;
  submittedA: boolean;
  submittedB: boolean;
  loserSeat: number | null;
  gameNumber: number;
  score: MatchScore;
}

export type DraftHostEvent =
  | { type: "seatJoined"; seatIndex: number; displayName: string }
  | { type: "seatReconnected"; seatIndex: number }
  | { type: "seatDisconnected"; seatIndex: number }
  | { type: "seatKicked"; seatIndex: number; reason: DraftPauseReason | string }
  | { type: "lobbyUpdate"; seats: SeatPublicView[]; joined: number; total: number }
  | { type: "lobbyFull" }
  | { type: "draftStarted"; view: DraftPlayerView }
  | { type: "pickReceived"; seatIndex: number; cardInstanceId: string }
  | { type: "roundComplete" }
  | { type: "draftComplete" }
  | { type: "deckSubmitted"; seatIndex: number }
  | { type: "allDecksSubmitted" }
  | { type: "draftPaused"; reason: DraftPauseReason }
  | { type: "draftResumed" }
  | { type: "error"; message: string }
  | { type: "viewUpdated"; view: DraftPlayerView }
  | { type: "pairingsGenerated"; round: number; pairings: PairingView[] }
  | { type: "matchStart"; launch: DraftMatchLaunch }
  | { type: "matchResultReceived"; matchId: string; winnerSeat: number | null }
  | { type: "roundAdvanced"; newRound: number }
  | { type: "timerExpired" }
  | {
      type: "bo3SideboardPrompt";
      matchId: string;
      gameNumber: number;
      score: MatchScore;
      loserSeat: number | null;
      timerMs: number;
    }
  | {
      type: "bo3ChoosePlayDraw";
      matchId: string;
      gameNumber: number;
      score: MatchScore;
      timerMs: number;
    }
  | { type: "bo3GameStart"; matchId: string; gameNumber: number; firstPlayerSeat: number }
  | { type: "bo3SideboardPromptSent"; matchId: string }
  | { type: "bo3BothSideboardsSubmitted"; matchId: string }
  | { type: "bo3GameStarted"; matchId: string; gameNumber: number };

type DraftHostEventListener = (event: DraftHostEvent) => void;

/** Default grace window for guest reconnect during draft. */
const DRAFT_GRACE_PERIOD_MS = 60_000;

/** Arena-style escalating pick timer durations (ms). Index = pick number (0-based). */
const PICK_TIMER_DURATIONS_MS: readonly number[] = [
  75_000, 70_000, 65_000, 58_000, 52_000, 46_000,
  40_000, 34_000, 28_000, 23_000, 20_000, 18_000, 16_000, 15_000,
];

function pickTimerDurationMs(pickNumber: number): number {
  return PICK_TIMER_DURATIONS_MS[Math.min(pickNumber, PICK_TIMER_DURATIONS_MS.length - 1)];
}

interface PickOptions {
  acknowledge?: boolean;
  emit?: boolean;
  persist?: boolean;
  resolveBots?: boolean;
}

interface ExportedDraftSession {
  pools?: Array<Array<{ name: string }>>;
  submitted_decks?: Record<string, { seat: number; main_deck: string[] }>;
}

function deckPayload(mainDeck: string[], sideboard: string[]): DraftDeckPayload {
  return { main_deck: mainDeck, sideboard, commander: [] };
}

function hashStringToSeed(value: string): number {
  let hash = 5381;
  for (let i = 0; i < value.length; i++) {
    hash = ((hash * 33) ^ value.charCodeAt(i)) | 0;
  }
  return hash >>> 0;
}

function sideboardFromPool(
  session: ExportedDraftSession,
  seat: number,
  mainDeck: string[],
): string[] {
  const counts = new Map<string, number>();
  for (const card of session.pools?.[seat] ?? []) {
    counts.set(card.name, (counts.get(card.name) ?? 0) + 1);
  }
  for (const name of mainDeck) {
    const count = counts.get(name);
    if (count === undefined) continue;
    if (count <= 1) counts.delete(name);
    else counts.set(name, count - 1);
  }
  return [...counts.entries()].flatMap(([name, count]) =>
    Array<string>(count).fill(name),
  );
}

// ── P2PDraftHost ───────────────────────────────────────────────────────

export class P2PDraftHost {
  private adapter = new DraftAdapter();
  private listeners: DraftHostEventListener[] = [];

  private guestSessions = new Map<number, DraftPeerSession>();
  private seatTokens = new Map<number, string>();
  private seatNames = new Map<number, string>();
  private kickedTokens = new Set<string>();
  private disconnectedSeats = new Map<
    number,
    { disconnectedAt: number; timer: ReturnType<typeof setTimeout> | null }
  >();
  private picksThisRound = new Set<number>();

  private draftStarted = false;
  private draftCode = "";
  private draftSeed: number | null = null;
  private activePodSize: number;
  private hostConnectionUnsub: (() => void) | null = null;
  private paused = false;
  private timerInterval: ReturnType<typeof setInterval> | null = null;
  private timerRemainingMs = 0;
  private timerEndAt = 0;
  private timerContext: "pick" | "sideboard" | "playdraw" | null = null;
  private bo3State = new Map<string, Bo3MatchState>();

  // Server backup upload state (D-08)
  private backupEndpoint: string | null = null;
  private picksSinceLastBackup = 0;
  private persistQueue = Promise.resolve();
  private persistenceClosed = false;
  private static readonly BACKUP_INTERVAL_PICKS = 5;

  constructor(
    private readonly hostPeer: Peer,
    private readonly onGuestConnected: (
      handler: (conn: DataConnection) => void,
    ) => () => void,
    private readonly poolInput: PoolInput,
    private readonly kind: "Premier" | "Traditional",
    private readonly podSize: number,
    private readonly hostDisplayName: string,
    private readonly tournamentFormat: TournamentFormat,
    private readonly podPolicy: PodPolicy,
    private readonly gracePeriodMs: number = DRAFT_GRACE_PERIOD_MS,
    private readonly persistenceId?: string,
    private readonly roomCode?: string,
    backupEndpoint?: string,
  ) {
    // Host is always seat 0
    this.seatNames.set(0, hostDisplayName);
    this.activePodSize = podSize;
    this.backupEndpoint = backupEndpoint ?? null;
  }

  // ── Event emitter ──────────────────────────────────────────────────

  onEvent(listener: DraftHostEventListener): () => void {
    this.listeners.push(listener);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener);
    };
  }

  private emit(event: DraftHostEvent): void {
    for (const listener of this.listeners) {
      listener(event);
    }
  }

  // ── Initialization ─────────────────────────────────────────────────

  async initialize(): Promise<void> {
    this.hostConnectionUnsub = this.onGuestConnected((conn) => {
      this.handleNewConnection(conn);
    });
    this.syncLobbyToGuests();
    this.persistSession();
  }

  // ── Connection handling ────────────────────────────────────────────

  private handleNewConnection(conn: DataConnection): void {
    const session = createDraftPeerSession(conn, {
      onSessionEnd: () => {
        for (const [seat, s] of this.guestSessions.entries()) {
          if (s === session) {
            this.handleGuestDisconnect(seat);
            return;
          }
        }
      },
    });

    let identified = false;
    const unsub = session.onMessage((msg) => {
      if (identified) return;
      identified = true;
      unsub();

      if (msg.type === "draft_join") {
        this.handleNewGuest(session, msg.displayName);
      } else if (msg.type === "draft_reconnect") {
        this.handleReconnect(session, msg.draftToken);
      } else {
        session.send({
          type: "draft_reconnect_rejected",
          reason: "Expected draft_join or draft_reconnect as first message",
        });
        session.close("Protocol violation");
      }
    });
  }

  private handleNewGuest(session: DraftPeerSession, displayName: string): void {
    if (this.draftStarted) {
      session.send({ type: "draft_kicked", reason: "Draft already in progress" });
      session.close("Draft in progress");
      return;
    }

    const seat = this.firstOpenSeat();
    if (seat === null) {
      session.send({ type: "draft_kicked", reason: "Pod is full" });
      session.close("Pod full");
      return;
    }

    const token = crypto.randomUUID();
    this.seatTokens.set(seat, token);
    this.guestSessions.set(seat, session);
    this.seatNames.set(seat, displayName);

    session.onMessage((msg) => this.handleGuestMessage(seat, msg));

    // Send welcome with empty view (draft hasn't started)
    const emptyView: DraftPlayerView = this.buildLobbyView();

    session.send({
      type: "draft_welcome",
      draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: token,
      seatIndex: seat,
      view: emptyView,
      draftCode: this.draftCode || "pending",
    });

    this.persistSession();
    this.emit({ type: "seatJoined", seatIndex: seat, displayName });
    this.syncLobbyToGuests();

    if (this.firstOpenSeat() === null) {
      this.emit({ type: "lobbyFull" });
    }
  }

  private handleReconnect(session: DraftPeerSession, draftToken: string): void {
    if (this.kickedTokens.has(draftToken)) {
      session.send({ type: "draft_reconnect_rejected", reason: "Player kicked" });
      session.close("Kicked");
      return;
    }

    let seat: number | null = null;
    for (const [s, token] of this.seatTokens) {
      if (token === draftToken) {
        seat = s;
        break;
      }
    }

    if (seat === null) {
      session.send({ type: "draft_reconnect_rejected", reason: "Unknown token" });
      session.close("Unknown token");
      return;
    }

    if (!this.disconnectedSeats.has(seat)) {
      session.send({
        type: "draft_reconnect_rejected",
        reason: "No grace window active for this seat",
      });
      session.close("Not in grace");
      return;
    }

    const grace = this.disconnectedSeats.get(seat)!;
    if (grace.timer !== null) clearTimeout(grace.timer);
    this.disconnectedSeats.delete(seat);
    this.guestSessions.set(seat, session);

    session.onMessage((msg) => this.handleGuestMessage(seat!, msg));

    // Send current view. Order matters: sync the engine connection bitmap
    // BEFORE fetching the view so the reconnect_ack carries the up-to-date
    // `seats[*].connected` snapshot. Then broadcast to siblings.
    void (async () => {
      try {
        if (this.draftStarted) {
          await this.adapter.setSeatConnected(seat!, true);
        }
        const view = this.draftStarted
          ? await this.adapter.getViewForSeat(seat!)
          : this.buildLobbyView();

        session.send({
          type: "draft_reconnect_ack",
          draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
          seatIndex: seat!,
          view,
          draftCode: this.draftCode,
        });
        if (this.draftStarted) {
          await this.broadcastViews();
        }
        if (view.status === "MatchInProgress") {
          await this.dispatchMatchLaunchesForSeat(view, seat!);
        }
      } catch (err) {
        console.error("[P2PDraftHost] reconnect view failed:", err);
      }
    })();

    for (const [otherSeat, otherSession] of this.guestSessions) {
      if (otherSeat === seat) continue;
      otherSession.send({
        type: "draft_lobby_update",
        seats: this.buildSeatPublicViews(),
        joined: this.occupiedSeatCount(),
        total: this.podSize,
      });
    }

    this.emit({ type: "seatReconnected", seatIndex: seat });

    // Resume if no other seats disconnected
    if (this.disconnectedSeats.size === 0 && this.paused) {
      this.paused = false;
      this.broadcastToGuests({ type: "draft_resumed" });
      this.emit({ type: "draftResumed" });
    }
  }

  // ── Message handling ───────────────────────────────────────────────

  private async handleGuestMessage(seat: number, msg: DraftP2PMessage): Promise<void> {
    switch (msg.type) {
      case "draft_pick": {
        if (!this.draftStarted || this.paused) {
          this.guestSessions.get(seat)?.send({
            type: "draft_error",
            reason: this.paused ? "Draft is paused" : "Draft not started",
          });
          return;
        }
        await this.handlePick(seat, msg.cardInstanceId);
        break;
      }
      case "draft_submit_deck": {
        if (!this.draftStarted) {
          this.guestSessions.get(seat)?.send({
            type: "draft_error",
            reason: "Draft not started",
          });
          return;
        }
        await this.handleDeckSubmission(seat, msg.mainDeck);
        break;
      }
      case "draft_match_result": {
        // T-57-06: validate matchId exists before processing
        await this.reportMatchResult(msg.matchId, msg.winnerSeat);
        break;
      }
      case "draft_request_advance": {
        // T-57-07: ignore from guests — only host UI triggers round advance
        break;
      }
      case "draft_bo3_sideboard_submit": {
        this.handleSideboardSubmit(seat, msg.matchId, msg.mainDeck, msg.sideboard);
        break;
      }
      case "draft_bo3_play_draw_choice": {
        this.handlePlayDrawChosen(seat, msg.matchId, msg.playFirst);
        break;
      }
      default:
        break;
    }
  }

  // ── Draft operations ───────────────────────────────────────────────

  /**
   * Start the draft. Called by the host UI once the pod is full
   * (or the host decides to start with fewer players).
   */
  async startDraft(botFillEmptySeats = true): Promise<void> {
    if (this.draftStarted) return;

    const seed = Math.floor(Math.random() * 0xffffffff);
    this.draftSeed = seed;
    const draftCode = `draft-${seed.toString(16).padStart(8, "0")}`;
    const seats: MultiplayerSeatDescriptor[] = [];
    for (let i = 0; i < this.podSize; i++) {
      const displayName = this.seatNames.get(i);
      if (displayName) {
        seats.push({
          type: "Human",
          player_id: i,
          display_name: displayName,
        });
      } else if (botFillEmptySeats) {
        seats.push({ type: "Bot", name: this.botNameForSeat(i, seed) });
      }
    }
    if (seats.length < 2) {
      throw new Error("Need at least two seats to start a pod draft");
    }

    await this.adapter.createMultiplayerDraft(
      this.poolInput,
      seats,
      this.kind,
      seed,
      draftCode,
      this.tournamentFormat,
      this.podPolicy,
    );

    this.draftStarted = true;
    this.draftCode = draftCode;
    this.activePodSize = seats.length;
    this.picksThisRound.clear();
    await this.resolveBotPicks({ emit: false, persist: false });

    // Send each guest their filtered view
    for (const [seat, session] of this.guestSessions) {
      try {
        const view = await this.adapter.getViewForSeat(seat);
        session.send({ type: "draft_state_update", view });
      } catch (err) {
        console.error(`[P2PDraftHost] Failed to send start view to seat ${seat}:`, err);
      }
    }

    this.persistSession();
    const freshHostView = await this.adapter.getViewForSeat(0);
    this.emit({ type: "draftStarted", view: freshHostView });
    this.startPickTimer(0);
  }

  /**
   * Host submits their own pick (seat 0).
   */
  async submitHostPick(cardInstanceId: string): Promise<DraftPlayerView> {
    return this.handlePick(0, cardInstanceId);
  }

  /**
   * Host submits their own deck (seat 0).
   */
  async submitHostDeck(mainDeck: string[]): Promise<DraftPlayerView> {
    return this.handleDeckSubmission(0, mainDeck);
  }

  private async handlePick(
    seat: number,
    cardInstanceId: string,
    resolveBots = true,
  ): Promise<DraftPlayerView> {
    return this.applyPick(seat, cardInstanceId, {
      acknowledge: true,
      emit: true,
      persist: true,
      resolveBots,
    });
  }

  private async applyPick(
    seat: number,
    cardInstanceId: string,
    options: PickOptions,
  ): Promise<DraftPlayerView> {
    try {
      const view = await this.adapter.submitPickForSeat(seat, cardInstanceId);
      this.picksThisRound.add(seat);

      // Send pick acknowledgement to the picking player
      const session = this.guestSessions.get(seat);
      if (options.acknowledge && session) {
        session.send({ type: "draft_pick_ack", view });
      }

      if (options.emit) {
        this.emit({ type: "pickReceived", seatIndex: seat, cardInstanceId });
      }
      if (options.persist) {
        this.persistSession();
      }

      if (options.resolveBots && !this.isBotSeat(seat)) {
        await this.resolveBotPicks({ emit: true, persist: true });
        await this.broadcastViews();
      }

      // Check if all picks for this round are in
      const allPicked = await this.adapter.allPicksSubmitted();
      if (allPicked) {
        this.picksThisRound.clear();
        this.clearActiveTimer();
        this.emit({ type: "roundComplete" });

        // Broadcast updated views to all players
        await this.broadcastViews();

        // Check if draft is complete (deckbuilding)
        const hostView = await this.adapter.getViewForSeat(0);
        if (hostView.status === "Deckbuilding") {
          this.clearActiveTimer();
          this.emit({ type: "draftComplete" });
        } else if (hostView.status === "Drafting") {
          this.startPickTimer(hostView.pick_number);
        }
      }

      // Return the host's updated view if this was the host's pick
      if (seat === 0) {
        return await this.adapter.getViewForSeat(0);
      }
      return await this.adapter.getViewForSeat(0);
    } catch (err) {
      const reason = err instanceof Error ? err.message : String(err);
      const session = this.guestSessions.get(seat);
      if (session) {
        session.send({ type: "draft_error", reason });
      }
      throw err;
    }
  }

  private async handleDeckSubmission(seat: number, mainDeck: string[]): Promise<DraftPlayerView> {
    try {
      const view = await this.adapter.submitDeckForSeat(seat, mainDeck);

      const session = this.guestSessions.get(seat);
      if (session) {
        session.send({ type: "draft_state_update", view });
      }

      this.emit({ type: "deckSubmitted", seatIndex: seat });
      this.persistSession();

      // Check if all decks are submitted
      const hostView = await this.adapter.getViewForSeat(0);
      if (hostView.seats.every((s) => s.has_submitted_deck || s.is_bot)) {
        this.emit({ type: "allDecksSubmitted" });
        await this.generatePairings(1);
      }

      if (seat === 0) return view;
      return hostView;
    } catch (err) {
      const reason = err instanceof Error ? err.message : String(err);
      const session = this.guestSessions.get(seat);
      if (session) {
        session.send({ type: "draft_error", reason });
      }
      throw err;
    }
  }

  // ── Broadcast ──────────────────────────────────────────────────────

  private async broadcastViews(): Promise<void> {
    for (const [seat, session] of this.guestSessions) {
      if (this.disconnectedSeats.has(seat)) continue;
      try {
        const view = await this.adapter.getViewForSeat(seat);
        await session.send({ type: "draft_state_update", view });
      } catch (err) {
        console.error(`[P2PDraftHost] broadcast view error seat ${seat}:`, err);
      }
    }
    // Update host's own view
    try {
      const hostView = await this.adapter.getViewForSeat(0);
      this.emit({ type: "viewUpdated", view: hostView });
    } catch { /* best-effort */ }
  }

  private broadcastToGuests(msg: DraftP2PMessage): void {
    for (const [seat, session] of this.guestSessions) {
      if (this.disconnectedSeats.has(seat)) continue;
      session.send(msg);
    }
  }

  private syncLobbyToGuests(): void {
    const joined = this.occupiedSeatCount();
    const total = this.podSize;
    const seats = this.buildSeatPublicViews();

    for (const session of this.guestSessions.values()) {
      session.send({
        type: "draft_lobby_update",
        seats,
        joined,
        total,
      });
    }

    this.emit({ type: "lobbyUpdate", seats, joined, total });
  }

  // ── Disconnect / Reconnect ─────────────────────────────────────────

  private handleGuestDisconnect(seat: number): void {
    if (!this.guestSessions.has(seat)) return;
    if (this.disconnectedSeats.has(seat)) return;

    this.guestSessions.delete(seat);

    if (!this.draftStarted) {
      // Pre-draft disconnect: free the seat
      this.seatTokens.delete(seat);
      this.seatNames.delete(seat);
      this.persistSession();
      this.syncLobbyToGuests();
      this.emit({ type: "seatDisconnected", seatIndex: seat });
      return;
    }

    // Mid-draft disconnect: sync the engine connection bitmap first so
    // `DraftPlayerView.seats[*].connected` reflects the new state, then
    // broadcast to all guests. Wrapped in a void-IIFE because this method
    // is sync `: void`; matching the existing convention on lines 342 / 1267.
    void (async () => {
      try {
        await this.adapter.setSeatConnected(seat, false);
        await this.broadcastViews();
      } catch (err) {
        console.error(
          `[P2PDraftHost] setSeatConnected(false) failed for seat ${seat}:`,
          err,
        );
      }
    })();

    // Mid-draft disconnect: grace window
    const timer = setTimeout(() => {
      // Grace expired — mark seat as abandoned but don't remove from draft
      // (other players' packs may depend on this seat's position)
      this.disconnectedSeats.delete(seat);
      this.emit({
        type: "seatKicked",
        seatIndex: seat,
        reason: DraftPauseReason.DisconnectGraceExpired,
      });
    }, this.gracePeriodMs);

    this.disconnectedSeats.set(seat, { disconnectedAt: Date.now(), timer });

    if (!this.paused) {
      this.paused = true;
      this.broadcastToGuests({
        type: "draft_paused",
        reason: DraftPauseReason.PlayerDisconnected,
      });
      this.emit({
        type: "draftPaused",
        reason: DraftPauseReason.PlayerDisconnected,
      });
    }

    this.emit({ type: "seatDisconnected", seatIndex: seat });
  }

  // ── Timer management ─────────────────────────────────────────────────

  private clearActiveTimer(): void {
    if (this.timerInterval !== null) {
      clearInterval(this.timerInterval);
      this.timerInterval = null;
    }
    this.timerContext = null;
  }

  private startPickTimer(pickNumber: number): void {
    this.clearActiveTimer();
    if (this.podPolicy !== "Competitive") return;
    this.timerContext = "pick";
    const duration = pickTimerDurationMs(pickNumber);
    this.timerRemainingMs = duration;
    this.timerEndAt = Date.now() + duration;
    this.timerInterval = setInterval(() => {
      this.onPickTimerTick();
    }, 1_000);
  }

  private onPickTimerTick(): void {
    this.timerRemainingMs = Math.max(0, this.timerEndAt - Date.now());
    this.broadcastToGuests({ type: "draft_timer_sync", remainingMs: this.timerRemainingMs });
    if (this.timerRemainingMs <= 0) {
      this.clearActiveTimer();
      this.emit({ type: "timerExpired" });
      void this.autoPickAllPending();
    }
  }

  private startSideboardTimer(matchId: string): void {
    this.clearActiveTimer();
    this.timerContext = "sideboard";
    const SIDEBOARD_TIMER_MS = 60_000;
    this.timerRemainingMs = SIDEBOARD_TIMER_MS;
    this.timerEndAt = Date.now() + SIDEBOARD_TIMER_MS;
    this.timerInterval = setInterval(() => {
      this.timerRemainingMs = Math.max(0, this.timerEndAt - Date.now());
      this.broadcastToGuests({ type: "draft_timer_sync", remainingMs: this.timerRemainingMs });
      if (this.timerRemainingMs <= 0) {
        this.clearActiveTimer();
        this.autoSubmitSideboards(matchId);
      }
    }, 1_000);
  }

  private startPlayDrawTimer(matchId: string): void {
    this.clearActiveTimer();
    this.timerContext = "playdraw";
    const PLAY_DRAW_TIMER_MS = 10_000;
    this.timerRemainingMs = PLAY_DRAW_TIMER_MS;
    this.timerEndAt = Date.now() + PLAY_DRAW_TIMER_MS;
    this.timerInterval = setInterval(() => {
      this.timerRemainingMs = Math.max(0, this.timerEndAt - Date.now());
      this.broadcastToGuests({ type: "draft_timer_sync", remainingMs: this.timerRemainingMs });
      if (this.timerRemainingMs <= 0) {
        this.clearActiveTimer();
        // Auto-choose "play" on expiry
        this.resolvePlayDrawChoice(matchId, true);
      }
    }, 1_000);
  }

  private async autoPickAllPending(): Promise<void> {
    // For each seat that still has a current_pack (hasn't picked), auto-pick
    // a random card (D-02). Skip seats already in `picksThisRound` — they've
    // already submitted this round and the engine would reject the duplicate
    // with `SeatAlreadyPickedThisRound`, swallowing the error and stranding
    // the timer at zero.
    //
    // Pass `resolveBots: false` to `handlePick` so the per-pick bot-pick
    // resolution and view broadcast are suppressed during the sweep. Otherwise
    // an N-seat sweep produces N redundant broadcasts (and N redundant bot
    // resolution sweeps). After the loop we resolve bots once and broadcast
    // once — except when the round naturally completed via `allPicksSubmitted`
    // inside the last `handlePick`, which already broadcast.
    let anyPicked = false;
    for (let seat = 0; seat < this.activePodSize; seat++) {
      if (this.picksThisRound.has(seat)) continue;
      try {
        const view = await this.adapter.getViewForSeat(seat);
        if (view.current_pack && view.current_pack.length > 0) {
          const randomIndex = Math.floor(Math.random() * view.current_pack.length);
          const card = view.current_pack[randomIndex];
          await this.handlePick(seat, card.instance_id, false);
          anyPicked = true;
        }
      } catch (err) {
        console.error(`[P2PDraftHost] auto-pick failed for seat ${seat}:`, err);
      }
    }
    if (anyPicked) {
      await this.resolveBotPicks({ emit: true, persist: true });
      const allPicked = await this.adapter.allPicksSubmitted();
      if (!allPicked) {
        await this.broadcastViews();
      }
    }
  }

  private async resolveBotPicks(options: PickOptions = { emit: true, persist: true }): Promise<void> {
    const hostView = await this.adapter.getViewForSeat(0);
    if (hostView.status !== "Drafting") return;

    for (const seat of hostView.seats) {
      if (!seat.is_bot) continue;
      const view = await this.adapter.getViewForSeat(seat.seat_index);
      const pack = view.current_pack;
      if (!pack || pack.length === 0) continue;

      const randomIndex = Math.floor(Math.random() * pack.length);
      await this.applyPick(
        seat.seat_index,
        pack[randomIndex].instance_id,
        { acknowledge: false, emit: options.emit, persist: options.persist, resolveBots: false },
      );
    }
  }

  private isBotSeat(seat: number): boolean {
    return this.seatNames.get(seat) === undefined && !this.guestSessions.has(seat);
  }

  private botNameForSeat(seat: number, seed: number): string {
    return assignAvatarForSeat(this.podSize, seat, seed)?.name ?? `Seat ${seat + 1}`;
  }

  // ── Match coordination ────────────────────────────────────────────────

  /**
   * Generate pairings for a given round and dispatch match start messages.
   * Called after all decks are submitted or after round advancement.
   */
  async generatePairings(round: number): Promise<void> {
    try {
      const view = await this.adapter.generatePairings(round);
      const launchablePairings = view.pairings.filter((pairing) =>
        pairing.round === round &&
        (pairing.status === "Pending" || pairing.status === "InProgress")
      );

      for (const pairing of launchablePairings) {
        if (
          this.isBotSeatFromView(view, pairing.seat_a) &&
          this.isBotSeatFromView(view, pairing.seat_b)
        ) {
          await this.dispatchMatchLaunch(pairing, view);
        }
      }

      const postBotView = await this.adapter.getViewForSeat(0);
      for (const pairing of postBotView.pairings) {
        if (pairing.round !== round) continue;
        if (pairing.status !== "Pending" && pairing.status !== "InProgress") continue;
        if (
          this.isBotSeatFromView(postBotView, pairing.seat_a) &&
          this.isBotSeatFromView(postBotView, pairing.seat_b)
        ) {
          continue;
        }

        await this.dispatchMatchLaunch(pairing, postBotView);
      }

      const latestView = await this.adapter.getViewForSeat(0);

      // Broadcast updated views
      await this.broadcastViews();
      this.persistSession();
      this.emit({ type: "pairingsGenerated", round, pairings: latestView.pairings });
      this.emit({ type: "viewUpdated", view: latestView });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.emit({ type: "error", message: `Failed to generate pairings: ${message}` });
    }
  }

  private async dispatchMatchLaunch(pairing: PairingView, view: DraftPlayerView): Promise<void> {
    const seatA = pairing.seat_a;
    const seatB = pairing.seat_b;
    const seatAIsBot = this.isBotSeatFromView(view, seatA);
    const seatBIsBot = this.isBotSeatFromView(view, seatB);
    const session = await this.exportDraftSession();

    if (seatAIsBot && seatBIsBot) {
      await this.reportMatchResult(pairing.match_id, Math.min(seatA, seatB));
      return;
    }

    if (seatAIsBot || seatBIsBot) {
      const humanSeat = seatAIsBot ? seatB : seatA;
      const botSeat = seatAIsBot ? seatA : seatB;
      const botName = seatAIsBot ? pairing.name_a : pairing.name_b;
      const humanDeck = this.submittedDeckForSeat(session, humanSeat);
      const botDeck = await this.botDeckForSeat(session, botSeat);
      const deckPayload: DraftMatchDeckPayload = {
        player: humanDeck,
        opponent: botDeck,
        ai_decks: [],
      };

      this.sendToSeat(humanSeat, {
        type: "draft_match_start",
        launch: {
          type: "Bot",
          matchId: pairing.match_id,
          round: pairing.round,
          localSeat: humanSeat,
          botSeat,
          botName,
          deckPayload,
          matchConfig: this.matchConfig(),
        },
      });
      return;
    }

    const matchHostSeat = Math.min(seatA, seatB);
    const guestSeat = matchHostSeat === seatA ? seatB : seatA;
    const matchRoomCode = `${this.draftCode ?? "draft"}-${pairing.match_id}`;
    const hostDeck = this.submittedDeckForSeat(session, matchHostSeat);
    const guestDeck = this.submittedDeckForSeat(session, guestSeat);
    const hostOpponentName = matchHostSeat === seatA ? pairing.name_b : pairing.name_a;
    const guestOpponentName = matchHostSeat === seatA ? pairing.name_a : pairing.name_b;
    const deckPayload: DraftMatchDeckPayload = {
      player: hostDeck,
      opponent: guestDeck,
      ai_decks: [],
    };

    this.sendToSeat(matchHostSeat, {
      type: "draft_match_start",
      launch: {
        type: "HumanHost",
        matchId: pairing.match_id,
        matchRoomCode,
        round: pairing.round,
        localSeat: matchHostSeat,
        opponentSeat: guestSeat,
        opponentName: hostOpponentName,
        matchHostPeerId: matchRoomCode,
        deckPayload,
        matchConfig: this.matchConfig(),
      },
    });
    this.sendToSeat(guestSeat, {
      type: "draft_match_start",
      launch: {
        type: "HumanGuest",
        matchId: pairing.match_id,
        matchRoomCode,
        round: pairing.round,
        localSeat: guestSeat,
        opponentSeat: matchHostSeat,
        opponentName: guestOpponentName,
        matchHostPeerId: matchRoomCode,
        localDeck: guestDeck,
        matchConfig: this.matchConfig(),
      },
    });
  }

  private async dispatchMatchLaunchesForSeat(view: DraftPlayerView, seat: number): Promise<void> {
    for (const pairing of view.pairings) {
      if (pairing.round !== view.current_round) continue;
      if (pairing.status !== "Pending" && pairing.status !== "InProgress") continue;
      if (pairing.seat_a !== seat && pairing.seat_b !== seat) continue;

      await this.dispatchMatchLaunch(pairing, view);
    }
  }

  private isBotSeatFromView(view: DraftPlayerView, seat: number): boolean {
    return view.seats.find((s) => s.seat_index === seat)?.is_bot ?? this.isBotSeat(seat);
  }

  private async exportDraftSession(): Promise<ExportedDraftSession> {
    const sessionJson = await this.adapter.exportSession();
    return JSON.parse(sessionJson) as ExportedDraftSession;
  }

  private submittedDeckForSeat(session: ExportedDraftSession, seat: number): DraftDeckPayload {
    const submitted = Object.values(session.submitted_decks ?? {}).find(
      (deck) => deck.seat === seat,
    );
    if (!submitted) {
      throw new Error(`Seat ${seat} has no submitted deck`);
    }
    return deckPayload(
      submitted.main_deck,
      sideboardFromPool(session, seat, submitted.main_deck),
    );
  }

  private async botDeckForSeat(
    session: ExportedDraftSession,
    botSeat: number,
  ): Promise<DraftDeckPayload> {
    const suggested = await this.adapter.getBotDeck(botSeat);
    const mainDeck = [
      ...suggested.main_deck,
      ...Object.entries(suggested.lands).flatMap(([name, count]) =>
        Array<string>(count).fill(name),
      ),
    ];
    return deckPayload(
      mainDeck,
      sideboardFromPool(session, botSeat, suggested.main_deck),
    );
  }

  private matchConfig(): MatchConfig {
    return { match_type: this.kind === "Traditional" ? "Bo3" : "Bo1" };
  }

  /**
   * Report a match result. Called when a guest sends draft_match_result.
   * T-57-06: validates matchId exists in current round pairings.
   */
  async reportMatchResult(matchId: string, winnerSeat: number | null): Promise<void> {
    try {
      const view = await this.adapter.reportMatchResult(matchId, winnerSeat);
      this.emit({ type: "matchResultReceived", matchId, winnerSeat });

      // Broadcast updated views with new standings
      await this.broadcastViews();
      this.persistSession();
      this.emit({ type: "viewUpdated", view });

      // Check if the reducer auto-advanced (Competitive mode)
      if (view.status === "Complete") {
        void this.cleanupServerBackup();
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      console.error(`[P2PDraftHost] reportMatchResult failed:`, message);
    }
  }

  /**
   * Advance to the next round (Casual mode, host-only).
   * T-57-07: only callable from host UI; guests sending draft_request_advance are ignored.
   */
  async advanceRound(): Promise<void> {
    try {
      const view = await this.adapter.advanceRound();
      const newRound = view.current_round;
      this.emit({ type: "roundAdvanced", newRound });
      await this.generatePairings(newRound);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.emit({ type: "error", message: `Failed to advance round: ${message}` });
    }
  }

  /**
   * Replace a disconnected player with a bot (Casual mode, host-only).
   */
  async replaceSeatWithBot(seat: number): Promise<void> {
    try {
      const seed = this.draftSeed ?? hashStringToSeed(this.draftCode || this.roomCode || "draft");
      await this.adapter.replaceSeatWithBot(seat, this.botNameForSeat(seat, seed));
      await this.broadcastViews();
      this.persistSession();
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.emit({ type: "error", message: `Failed to replace seat ${seat}: ${message}` });
    }
  }

  /**
   * Override a match result (Casual mode, host-only).
   */
  async overrideMatchResult(matchId: string, winnerSeat: number | null): Promise<void> {
    await this.reportMatchResult(matchId, winnerSeat);
  }

  // ── Bo3 Between-Games Orchestration ────────────────────────────────────

  /**
   * Orchestrates the between-games flow for a Bo3 match.
   * Called when the match adapter detects BetweenGamesSideboard waiting state.
   */
  handleMatchBetweenGames(
    matchId: string,
    gameNumber: number,
    score: MatchScore,
    loserSeat: number | null,
    seatA: number,
    seatB: number,
  ): void {
    this.bo3State.set(matchId, {
      seatA, seatB,
      submittedA: false, submittedB: false,
      loserSeat, gameNumber, score,
    });

    const timerMs = this.podPolicy === "Competitive" ? 60_000 : 0;

    // Send sideboard prompt to both pairing players via draft pod channel
    const prompt: DraftP2PMessage = {
      type: "draft_bo3_sideboard_prompt",
      matchId, gameNumber, score, loserSeat, timerMs,
    };
    this.sendToSeat(seatA, prompt);
    this.sendToSeat(seatB, prompt);

    // Broadcast live score to all guests for standings display
    this.broadcastToGuests({
      type: "draft_bo3_score_update",
      matchId,
      scoreA: score.p0_wins,
      scoreB: score.p1_wins,
    });

    if (timerMs > 0) {
      this.startSideboardTimer(matchId);
    }

    this.emit({ type: "bo3SideboardPromptSent", matchId });
  }

  /**
   * Handle a sideboard submission from a player in a Bo3 match.
   * T-58-01: validates seat matches seatA or seatB.
   */
  handleSideboardSubmit(
    seat: number,
    matchId: string,
    _mainDeck: string[],
    _sideboard: Array<{ name: string; count: number }>,
  ): void {
    const state = this.bo3State.get(matchId);
    if (!state) return;

    // T-58-01: validate sending seat belongs to this pairing
    if (seat === state.seatA) state.submittedA = true;
    else if (seat === state.seatB) state.submittedB = true;
    else return;

    // Check both-submitted gate
    if (state.submittedA && state.submittedB) {
      this.clearActiveTimer();
      this.emit({ type: "bo3BothSideboardsSubmitted", matchId });
      this.transitionToPlayDraw(matchId, state);
    }
  }

  /**
   * Handle play/draw choice from the losing player.
   * T-58-04: validates seat matches loserSeat.
   */
  handlePlayDrawChosen(seat: number, matchId: string, playFirst: boolean): void {
    const state = this.bo3State.get(matchId);
    if (!state || state.loserSeat !== seat) return;
    this.resolvePlayDrawChoice(matchId, playFirst);
  }

  private autoSubmitSideboards(matchId: string): void {
    const state = this.bo3State.get(matchId);
    if (!state) return;
    // Mark both as submitted (they keep their current decks)
    state.submittedA = true;
    state.submittedB = true;
    this.emit({ type: "bo3BothSideboardsSubmitted", matchId });
    this.transitionToPlayDraw(matchId, state);
  }

  private transitionToPlayDraw(matchId: string, state: Bo3MatchState): void {
    if (state.loserSeat !== null) {
      const timerMs = this.podPolicy === "Competitive" ? 10_000 : 0;
      const prompt: DraftP2PMessage = {
        type: "draft_bo3_play_draw_prompt",
        matchId,
        gameNumber: state.gameNumber,
        score: state.score,
        timerMs,
      };
      this.sendToSeat(state.loserSeat, prompt);
      if (timerMs > 0) this.startPlayDrawTimer(matchId);
    } else {
      // Draw — keep previous first player. Signal game start immediately.
      this.resolvePlayDrawChoice(matchId, true);
    }
  }

  private resolvePlayDrawChoice(matchId: string, playFirst: boolean): void {
    this.clearActiveTimer();
    const state = this.bo3State.get(matchId);
    if (!state) return;

    const firstPlayerSeat = playFirst
      ? (state.loserSeat ?? state.seatA)
      : (state.loserSeat === state.seatA ? state.seatB : state.seatA);

    const msg: DraftP2PMessage = {
      type: "draft_bo3_game_start",
      matchId,
      gameNumber: state.gameNumber,
      firstPlayerSeat,
    };
    this.sendToSeat(state.seatA, msg);
    this.sendToSeat(state.seatB, msg);

    this.bo3State.delete(matchId);
    this.emit({ type: "bo3GameStarted", matchId, gameNumber: state.gameNumber });
  }

  private sendToSeat(seat: number, msg: DraftP2PMessage): void {
    if (seat === 0) {
      // Host is seat 0 — emit event directly instead of sending over network
      switch (msg.type) {
        case "draft_match_start":
          this.emit({ type: "matchStart", launch: msg.launch });
          break;
        case "draft_bo3_sideboard_prompt":
          this.emit({
            type: "bo3SideboardPrompt",
            matchId: msg.matchId,
            gameNumber: msg.gameNumber,
            score: msg.score,
            loserSeat: msg.loserSeat,
            timerMs: msg.timerMs,
          });
          break;
        case "draft_bo3_play_draw_prompt":
          this.emit({
            type: "bo3ChoosePlayDraw",
            matchId: msg.matchId,
            gameNumber: msg.gameNumber,
            score: msg.score,
            timerMs: msg.timerMs,
          });
          break;
        case "draft_bo3_game_start":
          this.emit({
            type: "bo3GameStart",
            matchId: msg.matchId,
            gameNumber: msg.gameNumber,
            firstPlayerSeat: msg.firstPlayerSeat,
          });
          break;
        default:
          break;
      }
      return;
    }
    const session = this.guestSessions.get(seat);
    if (session && !this.disconnectedSeats.has(seat)) {
      session.send(msg);
    }
  }

  // ── Host controls ──────────────────────────────────────────────────

  kickPlayer(seat: number, reason: string = "Kicked by host"): void {
    const token = this.seatTokens.get(seat);
    if (token) this.kickedTokens.add(token);

    const session = this.guestSessions.get(seat);
    if (session) {
      session.send({ type: "draft_kicked", reason });
      session.close("Kicked");
      this.guestSessions.delete(seat);
    }

    // Cancel grace timer if active
    const grace = this.disconnectedSeats.get(seat);
    if (grace) {
      if (grace.timer !== null) clearTimeout(grace.timer);
      this.disconnectedSeats.delete(seat);
    }

    this.persistSession();
    this.emit({ type: "seatKicked", seatIndex: seat, reason });
    this.syncLobbyToGuests();
  }

  requestPause(): void {
    if (!this.paused) {
      this.clearActiveTimer();
      this.paused = true;
      this.broadcastToGuests({
        type: "draft_paused",
        reason: DraftPauseReason.PausedByHost,
      });
      this.emit({ type: "draftPaused", reason: DraftPauseReason.PausedByHost });
    }
  }

  requestResume(): void {
    if (this.paused && this.disconnectedSeats.size === 0) {
      this.paused = false;
      this.broadcastToGuests({ type: "draft_resumed" });
      this.emit({ type: "draftResumed" });
      // Restart timer if still in drafting phase
      if (this.draftStarted && this.podPolicy === "Competitive") {
        void (async () => {
          try {
            const view = await this.adapter.getViewForSeat(0);
            if (view.status === "Drafting") {
              this.startPickTimer(view.pick_number);
            }
          } catch { /* best-effort */ }
        })();
      }
    }
  }

  // ── Persistence (P2P-05) ──────────────────────────────────────────

  private persistSession(): void {
    if (!this.persistenceId || this.persistenceClosed) return;
    this.persistQueue = this.persistQueue.then(async () => {
      try {
        if (this.persistenceClosed) return;
        const sessionJson = this.draftStarted
          ? await this.adapter.exportSession()
          : null;
        if (this.persistenceClosed) return;

        const snapshot: PersistedDraftHostSession = {
          persistenceId: this.persistenceId!,
          roomCode: this.roomCode ?? "",
          kind: this.kind,
          podSize: this.podSize,
          hostDisplayName: this.hostDisplayName,
          tournamentFormat: this.tournamentFormat,
          podPolicy: this.podPolicy,
          seatTokens: Object.fromEntries(this.seatTokens),
          seatNames: Object.fromEntries(this.seatNames),
          kickedTokens: [...this.kickedTokens],
          draftStarted: this.draftStarted,
          draftCode: this.draftCode,
          draftSessionJson: sessionJson,
          poolInput: this.poolInput,
        };

        await saveDraftHostSession(this.persistenceId!, snapshot);

        // Server backup upload (D-08, T-60-11: rate-limited to every N picks)
        this.picksSinceLastBackup++;
        if (this.backupEndpoint && this.picksSinceLastBackup >= P2PDraftHost.BACKUP_INTERVAL_PICKS) {
          this.picksSinceLastBackup = 0;
          void this.uploadBackupSnapshot(snapshot);
        }
      } catch (err) {
        console.warn("[P2PDraftHost] persist failed:", err);
      }
    });
  }

  /**
   * Upload a backup snapshot to the phase-server (best-effort, D-08).
   * Failures are silently logged — P2P works without server backup.
   */
  private async uploadBackupSnapshot(snapshot: PersistedDraftHostSession): Promise<void> {
    if (!this.backupEndpoint || !this.draftCode) return;
    try {
      await fetch(`${this.backupEndpoint}/p2p-draft-backup`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          draft_code: this.draftCode,
          host_peer_id: this.hostPeer.id,
          snapshot_json: JSON.stringify(snapshot),
        }),
      });
    } catch (err) {
      console.warn("[P2PDraftHost] server backup upload failed:", err);
    }
  }

  /**
   * Delete the server backup on clean draft completion (best-effort).
   */
  private async cleanupServerBackup(): Promise<void> {
    if (!this.backupEndpoint || !this.draftCode) return;
    try {
      await fetch(`${this.backupEndpoint}/p2p-draft-backup/${this.draftCode}`, {
        method: "DELETE",
      });
    } catch {
      // Best-effort cleanup
    }
  }

  /**
   * Restore host state from a persisted snapshot.
   * Called before `initialize()` to rehydrate a crashed host.
   */
  async restoreFromPersisted(session: PersistedDraftHostSession): Promise<DraftPlayerView | null> {
    for (const [seatStr, token] of Object.entries(session.seatTokens)) {
      this.seatTokens.set(Number(seatStr), token);
    }
    for (const [seatStr, name] of Object.entries(session.seatNames)) {
      this.seatNames.set(Number(seatStr), name);
    }
    for (const token of session.kickedTokens) {
      this.kickedTokens.add(token);
    }
    this.draftStarted = session.draftStarted;
    this.draftCode = session.draftCode;
    this.draftSeed = hashStringToSeed(session.draftCode || this.roomCode || "draft");

    if (session.draftSessionJson) {
      const view = await this.adapter.importSession(session.draftSessionJson, 2);

      // Arm grace windows for all guest seats
      for (const seatStr of Object.keys(session.seatTokens)) {
        const seat = Number(seatStr);
        if (seat === 0) continue;
        const timer = setTimeout(() => {
          this.disconnectedSeats.delete(seat);
          this.emit({ type: "seatKicked", seatIndex: seat, reason: "Resume grace expired" });
        }, 5 * 60_000);
        this.disconnectedSeats.set(seat, { disconnectedAt: Date.now(), timer });
      }

      if (this.disconnectedSeats.size > 0) {
        this.paused = true;
        this.emit({
          type: "draftPaused",
          reason: DraftPauseReason.PlayerDisconnected,
        });
      }

      if (view.status === "MatchInProgress") {
        await this.dispatchMatchLaunchesForSeat(view, 0);
      } else if (view.status === "Pairing" && view.pairings.length === 0) {
        await this.generatePairings(view.current_round + 1);
        return this.adapter.getViewForSeat(0);
      }

      return view;
    }

    return null;
  }

  // ── Cleanup ────────────────────────────────────────────────────────

  dispose(): void {
    this.clearActiveTimer();
    if (this.hostConnectionUnsub) this.hostConnectionUnsub();
    for (const { timer } of this.disconnectedSeats.values()) {
      if (timer !== null) clearTimeout(timer);
    }
    this.disconnectedSeats.clear();
    this.bo3State.clear();
    for (const session of this.guestSessions.values()) {
      session.close();
    }
    this.guestSessions.clear();
    this.listeners = [];
  }

  async terminateDraft(): Promise<void> {
    for (const session of this.guestSessions.values()) {
      await session.send({ type: "draft_host_left", reason: "Host left the draft" });
    }
    this.persistenceClosed = true;
    await this.persistQueue;
    if (this.persistenceId) {
      await clearDraftHostSession(this.persistenceId);
    }
    void this.cleanupServerBackup();
    this.dispose();
    try {
      this.hostPeer.destroy();
    } catch { /* best-effort */ }
  }

  // ── Helpers ────────────────────────────────────────────────────────

  private firstOpenSeat(): number | null {
    for (let i = 1; i < this.podSize; i++) {
      if (!this.seatTokens.has(i)) return i;
    }
    return null;
  }

  private occupiedSeatCount(): number {
    // Host (seat 0) + connected guests
    return 1 + this.seatTokens.size - (this.seatTokens.has(0) ? 0 : 0);
  }

  private buildSeatPublicViews(): SeatPublicView[] {
    const seats: SeatPublicView[] = [];
    for (let i = 0; i < this.podSize; i++) {
      seats.push({
        seat_index: i,
        display_name: this.seatNames.get(i) ?? "",
        is_bot: false,
        connected: i === 0 || this.guestSessions.has(i),
        has_submitted_deck: false,
        pick_status: "NotDrafting",
      });
    }
    return seats;
  }

  private buildLobbyView(): DraftPlayerView {
    return {
      status: "Lobby",
      kind: this.kind,
      current_pack_number: 0,
      pick_number: 0,
      pass_direction: "Left",
      current_pack: null,
      pool: [],
      seats: this.buildSeatPublicViews(),
      cards_per_pack: 14,
      pack_count: 3,
      min_deck_size: 40,
      addable_cards: ["Plains", "Island", "Swamp", "Mountain", "Forest"],
      timer_remaining_ms: null,
      standings: [],
      current_round: 0,
      tournament_format: "Swiss",
      pod_policy: "Competitive",
      pairings: [],
    };
  }

  /** Get the host's current view. */
  async getHostView(): Promise<DraftPlayerView> {
    if (!this.draftStarted) return this.buildLobbyView();
    return this.adapter.getViewForSeat(0);
  }

  /** Whether the draft pod is full. */
  get isFull(): boolean {
    return this.firstOpenSeat() === null;
  }

  /** Whether the draft has started. */
  get isStarted(): boolean {
    return this.draftStarted;
  }

  /** Whether the draft is paused. */
  get isPaused(): boolean {
    return this.paused;
  }

  /** The active timer type, if any. */
  get activeTimerContext(): "pick" | "sideboard" | "playdraw" | null {
    return this.timerContext;
  }
}
