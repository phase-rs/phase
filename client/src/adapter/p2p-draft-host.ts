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
import type { DraftPlayerView, PairingView, SeatPublicView } from "./draft-adapter";
import type { PodPolicy } from "./draft-adapter";
import {
  createDraftPeerSession,
  type DraftPeerSession,
} from "../network/draftPeerSession";
import { DRAFT_PROTOCOL_VERSION } from "../network/draftProtocol";
import type { DraftP2PMessage } from "../network/draftProtocol";
import type { MatchScore } from "./types";
import {
  saveDraftHostSession,
  clearDraftHostSession,
  type PersistedDraftHostSession,
} from "../services/draftPersistence";

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
  | { type: "seatKicked"; seatIndex: number; reason: string }
  | { type: "lobbyUpdate"; joined: number; total: number }
  | { type: "lobbyFull" }
  | { type: "draftStarted"; view: DraftPlayerView }
  | { type: "pickReceived"; seatIndex: number; cardInstanceId: string }
  | { type: "roundComplete" }
  | { type: "draftComplete" }
  | { type: "deckSubmitted"; seatIndex: number }
  | { type: "allDecksSubmitted" }
  | { type: "error"; message: string }
  | { type: "viewUpdated"; view: DraftPlayerView }
  | { type: "pairingsGenerated"; round: number; pairings: PairingView[] }
  | { type: "matchResultReceived"; matchId: string; winnerSeat: number | null }
  | { type: "roundAdvanced"; newRound: number }
  | { type: "timerExpired" }
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

// ── P2PDraftHost ───────────────────────────────────────────────────────

export class P2PDraftHost {
  private adapter = new DraftAdapter();
  private listeners: DraftHostEventListener[] = [];

  private guestSessions = new Map<number, DraftPeerSession>();
  private seatTokens = new Map<number, string>();
  private seatNames = new Map<number, string>();
  private kickedTokens = new Set<string>();
  private seatPeerIds = new Map<number, string>();
  private disconnectedSeats = new Map<
    number,
    { disconnectedAt: number; timer: ReturnType<typeof setTimeout> | null }
  >();
  private picksThisRound = new Set<number>();

  private draftStarted = false;
  private draftCode = "";
  private hostConnectionUnsub: (() => void) | null = null;
  private paused = false;
  private timerInterval: ReturnType<typeof setInterval> | null = null;
  private timerRemainingMs = 0;
  private timerEndAt = 0;
  private timerContext: "pick" | "sideboard" | "playdraw" | null = null;
  private podPolicy: PodPolicy = "Competitive";
  private bo3State = new Map<string, Bo3MatchState>();

  // Server backup upload state (D-08)
  private backupEndpoint: string | null = null;
  private picksSinceLastBackup = 0;
  private static readonly BACKUP_INTERVAL_PICKS = 5;

  constructor(
    private readonly hostPeer: Peer,
    private readonly onGuestConnected: (
      handler: (conn: DataConnection) => void,
    ) => () => void,
    private readonly setPoolJson: string,
    private readonly kind: "Premier" | "Traditional",
    private readonly podSize: number,
    private readonly hostDisplayName: string,
    private readonly gracePeriodMs: number = DRAFT_GRACE_PERIOD_MS,
    private readonly persistenceId?: string,
    private readonly roomCode?: string,
    backupEndpoint?: string,
  ) {
    // Host is always seat 0
    this.seatNames.set(0, hostDisplayName);
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
  }

  // ── Connection handling ────────────────────────────────────────────

  private handleNewConnection(conn: DataConnection): void {
    const remotePeerId = conn.peer;
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
        this.handleNewGuest(session, msg.displayName, remotePeerId);
      } else if (msg.type === "draft_reconnect") {
        this.handleReconnect(session, msg.draftToken, remotePeerId);
      } else {
        session.send({
          type: "draft_reconnect_rejected",
          reason: "Expected draft_join or draft_reconnect as first message",
        });
        session.close("Protocol violation");
      }
    });
  }

  private handleNewGuest(session: DraftPeerSession, displayName: string, remotePeerId: string): void {
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
    this.seatPeerIds.set(seat, remotePeerId);

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

  private handleReconnect(session: DraftPeerSession, draftToken: string, remotePeerId: string): void {
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
    this.seatPeerIds.set(seat, remotePeerId);

    session.onMessage((msg) => this.handleGuestMessage(seat!, msg));

    // Send current view
    void (async () => {
      try {
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
  async startDraft(): Promise<void> {
    if (this.draftStarted) return;

    const seatNames: string[] = [];
    for (let i = 0; i < this.podSize; i++) {
      seatNames.push(this.seatNames.get(i) ?? `Player ${i + 1}`);
    }

    const seed = Math.floor(Math.random() * 0xffffffff);
    const hostView = await this.adapter.startMultiplayerDraft(
      this.setPoolJson,
      this.kind,
      seatNames,
      seed,
    );

    this.draftStarted = true;
    this.draftCode = `draft-${seed.toString(16).padStart(8, "0")}`;
    this.picksThisRound.clear();
    this.podPolicy = hostView.pod_policy;

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
    this.emit({ type: "draftStarted", view: hostView });
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

  private async handlePick(seat: number, cardInstanceId: string): Promise<DraftPlayerView> {
    try {
      const view = await this.adapter.submitPickForSeat(seat, cardInstanceId);
      this.picksThisRound.add(seat);

      // Send pick acknowledgement to the picking player
      const session = this.guestSessions.get(seat);
      if (session) {
        session.send({ type: "draft_pick_ack", view });
      }

      this.emit({ type: "pickReceived", seatIndex: seat, cardInstanceId });
      this.persistSession();

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
        return view;
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

    this.emit({ type: "lobbyUpdate", joined, total });
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

    // Mid-draft disconnect: grace window
    const timer = setTimeout(() => {
      // Grace expired — mark seat as abandoned but don't remove from draft
      // (other players' packs may depend on this seat's position)
      this.disconnectedSeats.delete(seat);
      this.emit({ type: "seatKicked", seatIndex: seat, reason: "Disconnect grace expired" });
    }, this.gracePeriodMs);

    this.disconnectedSeats.set(seat, { disconnectedAt: Date.now(), timer });

    if (!this.paused) {
      this.paused = true;
      this.broadcastToGuests({ type: "draft_paused", reason: "Player disconnected" });
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
    // For each seat that still has a current_pack (hasn't picked), auto-pick a random card (D-02)
    for (let seat = 0; seat < this.podSize; seat++) {
      try {
        const view = await this.adapter.getViewForSeat(seat);
        if (view.current_pack && view.current_pack.length > 0) {
          const randomIndex = Math.floor(Math.random() * view.current_pack.length);
          const card = view.current_pack[randomIndex];
          await this.handlePick(seat, card.instance_id);
        }
      } catch (err) {
        console.error(`[P2PDraftHost] auto-pick failed for seat ${seat}:`, err);
      }
    }
  }

  // ── Match coordination ────────────────────────────────────────────────

  /**
   * Generate pairings for a given round and dispatch match start messages.
   * Called after all decks are submitted or after round advancement.
   */
  async generatePairings(round: number): Promise<void> {
    try {
      const view = await this.adapter.generatePairings(round);
      this.podPolicy = view.pod_policy;

      // Send draft_match_start to each guest with their opponent info
      for (const pairing of view.pairings) {
        if (pairing.round !== round) continue;
        if (pairing.status !== "Pending" && pairing.status !== "InProgress") continue;

        const seatA = pairing.seat_a;
        const seatB = pairing.seat_b;
        // Lower seat# is match host
        const matchHostSeat = Math.min(seatA, seatB);
        const matchHostPeerId = this.getPeerIdForSeat(matchHostSeat);

        // Send to seat A
        this.sendToSeat(seatA, {
          type: "draft_match_start",
          matchId: pairing.match_id,
          round: pairing.round,
          opponentSeat: seatB,
          opponentName: pairing.name_b,
          matchHostPeerId,
          isMatchHost: seatA === matchHostSeat,
        });

        // Send to seat B
        this.sendToSeat(seatB, {
          type: "draft_match_start",
          matchId: pairing.match_id,
          round: pairing.round,
          opponentSeat: seatA,
          opponentName: pairing.name_a,
          matchHostPeerId,
          isMatchHost: seatB === matchHostSeat,
        });
      }

      // Broadcast updated views
      await this.broadcastViews();
      this.emit({ type: "pairingsGenerated", round, pairings: view.pairings });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.emit({ type: "error", message: `Failed to generate pairings: ${message}` });
    }
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

      // Check if the reducer auto-advanced (Competitive mode)
      if (view.status === "RoundComplete" || view.status === "Complete") {
        const hostView = await this.adapter.getViewForSeat(0);
        this.emit({ type: "viewUpdated", view: hostView });
        if (view.status === "Complete") {
          void this.cleanupServerBackup();
        }
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
      await this.adapter.replaceSeatWithBot(seat);
      await this.broadcastViews();
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
      return;
    }
    const session = this.guestSessions.get(seat);
    if (session && !this.disconnectedSeats.has(seat)) {
      session.send(msg);
    }
  }

  private getPeerIdForSeat(seat: number): string {
    if (seat === 0) return this.hostPeer.id;
    return this.seatPeerIds.get(seat) ?? "";
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
      this.broadcastToGuests({ type: "draft_paused", reason: "Paused by host" });
    }
  }

  requestResume(): void {
    if (this.paused && this.disconnectedSeats.size === 0) {
      this.paused = false;
      this.broadcastToGuests({ type: "draft_resumed" });
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
    if (!this.persistenceId) return;
    void (async () => {
      try {
        const sessionJson = this.draftStarted
          ? await this.adapter.exportSession()
          : null;

        const snapshot: PersistedDraftHostSession = {
          persistenceId: this.persistenceId!,
          roomCode: this.roomCode ?? "",
          kind: this.kind,
          podSize: this.podSize,
          hostDisplayName: this.hostDisplayName,
          seatTokens: Object.fromEntries(this.seatTokens),
          seatNames: Object.fromEntries(this.seatNames),
          kickedTokens: [...this.kickedTokens],
          draftStarted: this.draftStarted,
          draftCode: this.draftCode,
          draftSessionJson: sessionJson,
          setPoolJson: this.setPoolJson,
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
    })();
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

    if (session.draftSessionJson) {
      const view = await this.adapter.importSession(session.draftSessionJson);

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
    if (this.persistenceId) {
      void clearDraftHostSession(this.persistenceId);
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
