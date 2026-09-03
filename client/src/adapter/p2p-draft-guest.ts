/**
 * P2P Draft Tournament Guest.
 *
 * Connects to a P2PDraftHost, receives filtered draft views, and
 * submits picks and deck lists. Persists the draft token in IndexedDB
 * for reconnection (P2P-04, P2P-07).
 *
 * Mirrors `P2PGuestAdapter` architecture: the guest holds no
 * authoritative state — everything is server-said (host-said).
 */

import type Peer from "peerjs";
import type { DataConnection } from "peerjs";

import type { DraftPlayerView, SeatPublicView } from "./draft-adapter";
import {
  createDraftPeerSession,
  type DraftPeerSession,
} from "../network/draftPeerSession";
import { parseRoomCode } from "../network/connection";
import {
  deckSubmissionFingerprint,
  DRAFT_PROTOCOL_VERSION,
  type DraftReconnectRejectionKind,
} from "../network/draftProtocol";
import type {
  DraftMatchLaunch,
  DraftMatchSettlement,
  DraftP2PMessage,
  DraftPauseReason,
} from "../network/draftProtocol";
import {
  saveDraftGuestSession,
  clearDraftGuestRecovery,
  saveActiveDraftGuest,
  clearDraftDeckSubmission,
  loadDraftDeckSubmission,
  saveDraftDeckSubmission,
} from "../services/draftPersistence";
import type {
  DraftIntergameCommand,
  DraftIntergameCommandAck,
} from "../services/intergameCommandLedger";
import {
  validateWorkspaceState,
  type DraftWorkspaceState,
} from "../components/draft/workspace/types";

// ── Types ──────────────────────────────────────────────────────────────

export type DraftGuestEvent =
  | { type: "joined"; seatIndex: number; draftCode: string }
  | { type: "reconnected"; seatIndex: number }
  | { type: "workspaceRestored"; workspaceState: DraftWorkspaceState | null }
  | { type: "viewUpdated"; view: DraftPlayerView }
  | { type: "pickAcknowledged"; view: DraftPlayerView }
  | { type: "deckSubmissionAcknowledged"; submissionId: string; view: DraftPlayerView }
  | { type: "lobbyUpdate"; seats: SeatPublicView[]; joined: number; total: number }
  | { type: "draftPaused"; reason: DraftPauseReason }
  | { type: "draftResumed" }
  | { type: "pairing"; round: number; table: number; opponentName: string; matchHostPeerId: string; matchId: string }
  | { type: "matchResult"; matchId: string; winnerSeat: number | null }
  | { type: "matchSettlementAcknowledged"; matchId: string; receiptId: string; revision: number }
  | { type: "timerSync"; remainingMs: number }
  | { type: "matchStart"; launch: DraftMatchLaunch }
  | { type: "bo3SideboardPrompt"; matchId: string; gameNumber: number; score: { p0_wins: number; p1_wins: number; draws: number }; loserSeat: number | null; timerMs: number }
  | { type: "bo3ChoosePlayDraw"; matchId: string; gameNumber: number; score: { p0_wins: number; p1_wins: number; draws: number }; timerMs: number }
  | { type: "bo3GameStart"; matchId: string; gameNumber: number; firstPlayerSeat: number }
  | { type: "bo3AuthorizedCommand"; command: DraftIntergameCommand; acknowledgement: DraftIntergameCommandAck }
  | { type: "bo3ScoreUpdate"; matchId: string; scoreA: number; scoreB: number }
  | { type: "kicked"; reason: string }
  | { type: "hostLeft"; reason: string }
  | { type: "error"; message: string }
  | { type: "reconnecting"; attempt: number }
  | { type: "reconnectFailed"; failure: DraftGuestRecoveryFailure };

/**
 * The recovery layer, not a rendered string, owns whether another explicit
 * reconnect attempt is meaningful.  This keeps terminal capability revocation
 * and an offline host from sharing a misleading "Retry" affordance.
 */
export type DraftGuestRecoveryFailure =
  | { kind: "retryable"; message: string }
  | { kind: "incompatible"; message: string }
  | { kind: "invalid"; message: string };

/** First contact is intentionally an exclusive choice, never token fallback. */
export type DraftGuestConnection =
  | {
      kind: "new";
      roomCode: string;
      displayName: string;
    }
  | {
      kind: "reconnect";
      roomCode: string;
      displayName: string;
      draftToken: string;
    };

type DraftGuestEventListener = (event: DraftGuestEvent) => void;

const RECONNECT_BACKOFF_MS = [1_000, 2_000, 4_000, 8_000, 15_000, 30_000, 60_000];
const RECONNECT_STEADY_STATE_MS = 60_000;
const FIRST_CONTACT_TIMEOUT_MS = 10_000;
const LEAVE_ACK_TIMEOUT_MS = 10_000;

function reconnectFailureForRejection(
  kind: DraftReconnectRejectionKind,
  message: string,
): DraftGuestRecoveryFailure {
  switch (kind) {
    case "Kicked":
    case "UnknownToken":
      return { kind: "invalid", message };
    case "ProtocolMismatch":
      return { kind: "incompatible", message };
    case "NoReconnectWindow":
      return { kind: "retryable", message };
  }
}

interface DraftHandshake {
  session: DraftPeerSession;
  resolve: () => void;
  reject: (reason: Error) => void;
  cleanup: () => void;
}

// ── P2PDraftGuest ──────────────────────────────────────────────────────

export class P2PDraftGuest {
  private listeners: DraftGuestEventListener[] = [];
  private session: DraftPeerSession | null = null;
  private draftToken: string | null = null;
  private draftCode: string | null = null;
  private seatIndex: number | null = null;
  private terminated = false;
  /**
   * A host may explicitly revoke the persisted capability (kick, terminal
   * host shutdown, or an acknowledged leave).  This is intentionally
   * narrower than `terminated`: a protocol mismatch is terminal for this
   * transport attempt but should retain recovery for a refreshed client.
   */
  private recoveryRevoked = false;
  private currentView: DraftPlayerView | null = null;
  private handshake: DraftHandshake | null = null;
  private reconnecting = false;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private deckSubmissionWaiters = new Map<
    string,
    {
      acknowledgement: Promise<void>;
      resolve: () => void;
      reject: (error: Error) => void;
      activeAttempts: number;
    }
  >();
  /** Set synchronously so two UI clicks share one outbox command. */
  private pendingDeckSubmission: Promise<void> | null = null;
  private pendingLeave: Promise<void> | null = null;
  private leaveAcknowledgement: {
    session: DraftPeerSession;
    draftToken: string;
    resolve: () => void;
    reject: (error: Error) => void;
  } | null = null;

  constructor(
    private readonly guestPeer: Peer,
    private readonly hostPeerId: string,
    private readonly initialConn: DataConnection,
    private readonly connection: DraftGuestConnection,
  ) {
    if (connection.kind === "reconnect") {
      this.draftToken = connection.draftToken;
    }
  }

  // ── Event emitter ──────────────────────────────────────────────────

  onEvent(listener: DraftGuestEventListener): () => void {
    this.listeners.push(listener);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener);
    };
  }

  private emit(event: DraftGuestEvent): void {
    for (const listener of this.listeners) {
      listener(event);
    }
  }

  // ── Initialization ─────────────────────────────────────────────────

  async initialize(signal?: AbortSignal, reconnectAttemptLimit = Number.POSITIVE_INFINITY): Promise<void> {
    // A new guest gets exactly one join attempt. Only a persisted capability
    // may use retry, and every retry waits for a complete host acknowledgement.
    if (this.connection.kind === "new") {
      await this.handshakeOn(this.initialConn, signal, false);
      return;
    }

    let conn = this.initialConn;
    for (let attempt = 0; !this.terminated && attempt < reconnectAttemptLimit; attempt++) {
      try {
        await this.handshakeOn(conn, signal, true);
        return;
      } catch (err) {
        if (this.terminated || signal?.aborted) throw asError(err);
        if (attempt + 1 >= reconnectAttemptLimit) throw asError(err);
        this.emit({ type: "reconnecting", attempt: attempt + 1 });
        await this.waitForRetry(attempt, signal);
        if (this.terminated || signal?.aborted) throw abortError();
        conn = await this.openReconnectConnection(signal);
      }
    }
    throw abortError();
  }

  private attachSession(conn: DataConnection): DraftPeerSession {
    const session = createDraftPeerSession(conn, {
      onSessionEnd: () => {
        this.handleSessionEnd(session);
      },
    });
    this.session = session;
    session.onMessage((msg) => {
      // A timed-out or superseded connection must never promote a later
      // reconnect attempt with its delayed acknowledgement.
      if (this.session === session) return this.handleHostMessage(msg, session);
    });
    return session;
  }

  private async handshakeOn(conn: DataConnection, signal?: AbortSignal, reconnect = true): Promise<void> {
    if (signal?.aborted) throw abortError();
    if (this.session) this.retireSession(this.session);
    const session = this.attachSession(conn);
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.rejectHandshake(session, new Error("Draft host did not acknowledge connection"));
      }, FIRST_CONTACT_TIMEOUT_MS);
      const onAbort = () => this.rejectHandshake(session, abortError());
      signal?.addEventListener("abort", onAbort, { once: true });
      this.handshake = {
        session,
        resolve,
        reject,
        cleanup: () => {
          clearTimeout(timer);
          signal?.removeEventListener("abort", onAbort);
        },
      };
      void this.sendFirstContact(session, reconnect)
        .catch((err: unknown) => this.rejectHandshake(session, asError(err)));
    });
  }

  private sendFirstContact(session: DraftPeerSession, reconnect: boolean): Promise<void> {
    if (!reconnect) {
      return session.send({
        type: "draft_join",
        displayName: this.connection.displayName,
        draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      });
    }
    if (!this.draftToken) return Promise.reject(new Error("Draft reconnect token is unavailable"));
    return session.send({
      type: "draft_reconnect",
      draftToken: this.draftToken,
      draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
    });
  }

  private resolveHandshake(session: DraftPeerSession): void {
    const handshake = this.handshake;
    if (!handshake || handshake.session !== session) return;
    this.handshake = null;
    handshake.cleanup();
    handshake.resolve();
  }

  private rejectHandshake(session: DraftPeerSession, reason: Error): void {
    const handshake = this.handshake;
    if (!handshake || handshake.session !== session) return;
    this.handshake = null;
    handshake.cleanup();
    this.retireSession(session);
    handshake.reject(reason);
  }

  private retireSession(session: DraftPeerSession): void {
    if (this.session === session) this.session = null;
    session.close("Draft reconnect attempt retired");
  }

  private handleSessionEnd(session: DraftPeerSession): void {
    if (this.session !== session) return;
    this.session = null;
    if (this.handshake?.session === session) {
      this.rejectHandshake(session, new Error("Draft host disconnected before acknowledging"));
    } else {
      if (this.leaveAcknowledgement?.session === session) {
        this.leaveAcknowledgement.reject(new Error("Draft host disconnected before acknowledging leave"));
        this.leaveAcknowledgement = null;
      }
      this.handleHostDisconnect();
    }
  }

  // ── Actions ────────────────────────────────────────────────────────

  /** Submit one whole CR 903.13b pick step — every card this seat drafts now. */
  async submitPick(cardInstanceIds: string[]): Promise<void> {
    if (!this.session) throw new Error("Not connected to draft host");
    await this.session.send({ type: "draft_pick", cardInstanceIds });
  }

  async submitPickWithDraftEffect(
    effectCardInstanceId: string,
    cardInstanceIds: string[],
  ): Promise<void> {
    if (!this.session) throw new Error("Not connected to draft host");
    await this.session.send({
      type: "draft_pick_with_draft_effect",
      effectCardInstanceId,
      cardInstanceIds,
    });
  }

  submitDeck(mainDeck: string[], commanders: string[]): Promise<void> {
    if (this.pendingDeckSubmission) return this.pendingDeckSubmission;
    const submission = this.submitDeckInner(mainDeck, commanders);
    this.pendingDeckSubmission = submission;
    void submission.then(
      () => {
        if (this.pendingDeckSubmission === submission) this.pendingDeckSubmission = null;
      },
      () => {
        if (this.pendingDeckSubmission === submission) this.pendingDeckSubmission = null;
      },
    );
    return submission;
  }

  private async submitDeckInner(mainDeck: string[], commanders: string[]): Promise<void> {
    const identity = this.deckSubmissionIdentity();
    if (!identity) throw new Error("Draft identity is unavailable");
    const existing = await loadDraftDeckSubmission(this.hostPeerId, identity);
    // CR 903.3: the designation is part of the payload's identity, not a
    // decoration on it. Fingerprinting `mainDeck` alone would let a resubmit
    // that changes only the commander read as "same payload" and silently
    // replay the STORED designation, discarding the player's change.
    const samePayload = existing !== null
      && deckSubmissionFingerprint(existing.mainDeck) === deckSubmissionFingerprint(mainDeck)
      && deckSubmissionFingerprint(existing.commanders) === deckSubmissionFingerprint(commanders);
    if (existing && !samePayload) {
      throw new Error("A deck submission is still awaiting host confirmation");
    }
    const submissionId = existing?.submissionId ?? crypto.randomUUID();
    const payload = existing?.mainDeck ?? mainDeck;
    const designation = existing?.commanders ?? commanders;
    if (!existing) {
      await saveDraftDeckSubmission(this.hostPeerId, {
        ...identity,
        draftCode: this.draftCode!,
        submissionId,
        mainDeck: payload,
        commanders: designation,
      });
    }
    await this.sendDeckSubmission(submissionId, payload, designation);
  }

  private async sendDeckSubmission(
    submissionId: string,
    mainDeck: string[],
    commanders: string[],
  ): Promise<void> {
    if (!this.session) throw new Error("Not connected to draft host");
    let waiter = this.deckSubmissionWaiters.get(submissionId);
    if (!waiter) {
      let resolve!: () => void;
      let reject!: (error: Error) => void;
      const acknowledgement = new Promise<void>((resolvePromise, rejectPromise) => {
        resolve = resolvePromise;
        reject = rejectPromise;
      });
      waiter = { acknowledgement, resolve, reject, activeAttempts: 0 };
      this.deckSubmissionWaiters.set(submissionId, waiter);
    }
    waiter.activeAttempts += 1;
    try {
      // Observe the receipt even if the session closes while encoding the send.
      await Promise.all([
        this.session.send({ type: "draft_submit_deck", submissionId, mainDeck, commanders }),
        waiter.acknowledgement,
      ]);
    } finally {
      // A failed replay must not remove the receipt route used by other attempts.
      waiter.activeAttempts -= 1;
      if (waiter.activeAttempts === 0 && this.deckSubmissionWaiters.get(submissionId) === waiter) {
        this.deckSubmissionWaiters.delete(submissionId);
      }
    }
  }

  private failDeckSubmissionWaiters(reason: string): void {
    for (const waiter of this.deckSubmissionWaiters.values()) {
      waiter.reject(new Error(reason));
    }
    this.deckSubmissionWaiters.clear();
  }

  /** A reconnect makes the participant-owned command eligible for replay. */
  private async replayDeckSubmission(): Promise<void> {
    const identity = this.deckSubmissionIdentity();
    if (!identity) return;
    const pending = await loadDraftDeckSubmission(this.hostPeerId, identity);
    if (!pending || !this.session) return;
    // Do not await here: the reconnect handshake must finish before normal
    // state consumers run, while its durable submission can wait for its ack.
    void this.sendDeckSubmission(pending.submissionId, pending.mainDeck, pending.commanders)
      .catch((error: unknown) => this.emit({
        type: "error",
        message: error instanceof Error ? error.message : String(error),
      }));
  }

  private deckSubmissionIdentity(): { roomCode: string; draftToken: string } | null {
    const roomCode = parseRoomCode(this.connection.roomCode);
    if (!this.draftCode || !this.draftToken || !roomCode) return null;
    return { roomCode, draftToken: this.draftToken };
  }

  async updateWorkspace(state: DraftWorkspaceState): Promise<void> {
    const validated = validateWorkspaceState(state);
    if ("error" in validated) throw new Error(validated.error);
    if (!this.session) throw new Error("Not connected to draft host");
    await this.session.send({ type: "draft_workspace_update", workspaceState: validated });
  }

  sendMatchSettlement(settlement: DraftMatchSettlement): void {
    if (!this.session) return;
    void this.session.send({ type: "draft_match_settlement", settlement });
  }

  sendBetweenGames(
    matchId: string,
    gameNumber: number,
    score: { p0_wins: number; p1_wins: number; draws: number },
    loserSeat: number | null,
  ): void {
    if (!this.session) return;
    void this.session.send({ type: "draft_bo3_between_games", matchId, gameNumber, score, loserSeat });
  }

  sendAuthorizedIntergameCommand(command: DraftIntergameCommand): void {
    if (!this.session) return;
    void this.session.send({ type: "draft_bo3_intergame_command", command });
  }

  sendIntergameReceipt(acknowledgement: DraftIntergameCommandAck, receiptId: string): void {
    if (!this.session) return;
    void this.session.send({ type: "draft_bo3_intergame_receipt", acknowledgement, receiptId });
  }

  // ── Message handling ───────────────────────────────────────────────

  private async handleHostMessage(msg: DraftP2PMessage, session: DraftPeerSession): Promise<void> {
    // Protocol version check on first-contact messages
    if (msg.type === "draft_welcome" || msg.type === "draft_reconnect_ack") {
      if (msg.draftProtocolVersion !== DRAFT_PROTOCOL_VERSION) {
        const reason = `Draft protocol mismatch: host v${msg.draftProtocolVersion}, client v${DRAFT_PROTOCOL_VERSION}. Refresh both windows.`;
        console.error("[P2PDraftGuest]", reason);
        this.terminated = true;
        this.rejectHandshake(session, new Error(reason));
        this.emit({
          type: "reconnectFailed",
          failure: { kind: "incompatible", message: reason },
        });
        return;
      }
    }

    switch (msg.type) {
      case "draft_welcome": {
        this.seatIndex = msg.seatIndex;
        this.draftToken = msg.draftToken;
        this.draftCode = msg.draftCode;
        this.currentView = msg.view;

        try {
          // The IDB capability must commit before its local-storage locator is
          // published; otherwise a reload can observe a dead recovery route.
          await this.persistRecoveryIdentity({
            draftToken: msg.draftToken,
            seatIndex: msg.seatIndex,
            draftCode: msg.draftCode,
          });
        } catch (err) {
          this.rejectHandshake(session, asError(err));
          this.emit({ type: "error", message: "Could not save draft recovery details" });
          break;
        }

        // Persistence can outlive a disconnected or retired handshake.
        if (this.session !== session) return;
        this.resolveHandshake(session);
        this.emit({ type: "workspaceRestored", workspaceState: msg.workspaceState });
        this.emit({ type: "joined", seatIndex: msg.seatIndex, draftCode: msg.draftCode });
        this.emit({ type: "viewUpdated", view: msg.view });
        void this.replayDeckSubmission();
        break;
      }

      case "draft_reconnect_ack": {
        this.seatIndex = msg.seatIndex;
        this.draftCode = msg.draftCode;
        this.currentView = msg.view;

        if (this.draftToken) {
          try {
            await this.persistRecoveryIdentity({
              draftToken: this.draftToken,
              seatIndex: msg.seatIndex,
              draftCode: msg.draftCode,
            });
          } catch (err) {
            this.rejectHandshake(session, asError(err));
            this.emit({ type: "error", message: "Could not save draft recovery details" });
            break;
          }
        }

        if (this.session !== session) return;
        this.resolveHandshake(session);
        this.emit({ type: "workspaceRestored", workspaceState: msg.workspaceState });
        this.emit({ type: "reconnected", seatIndex: msg.seatIndex });
        this.emit({ type: "viewUpdated", view: msg.view });
        void this.replayDeckSubmission();
        break;
      }

      case "draft_reconnect_rejected": {
        this.rejectHandshake(session, new Error(msg.reason));
        if (msg.kind === "Kicked" || msg.kind === "UnknownToken") {
          this.terminated = true;
          await this.revokeRecovery();
        } else if (msg.kind === "ProtocolMismatch") {
          // Refresh can restore compatibility, so retain credentials, but a
          // version mismatch cannot be repaired by transport retries.
          this.terminated = true;
        }
        this.emit({
          type: "reconnectFailed",
          failure: reconnectFailureForRejection(msg.kind, msg.reason),
        });
        break;
      }

      case "draft_leave_ack": {
        this.resolveLeaveAcknowledgement(session, msg.draftToken);
        break;
      }

      case "draft_state_update": {
        this.currentView = msg.view;
        this.emit({ type: "viewUpdated", view: msg.view });
        break;
      }

      case "draft_pick_ack": {
        this.currentView = msg.view;
        this.emit({ type: "pickAcknowledged", view: msg.view });
        break;
      }

      case "draft_deck_submit_ack": {
        this.currentView = msg.view;
        await clearDraftDeckSubmission(this.hostPeerId, msg.submissionId);
        this.deckSubmissionWaiters.get(msg.submissionId)?.resolve();
        // The durable receipt settles its caller even if the session closed,
        // but its old view must not be published into a reconnect attempt.
        if (this.session !== session) return;
        this.emit({ type: "deckSubmissionAcknowledged", submissionId: msg.submissionId, view: msg.view });
        this.emit({ type: "viewUpdated", view: msg.view });
        break;
      }

      case "draft_error": {
        if (msg.submissionId) {
          const waiter = this.deckSubmissionWaiters.get(msg.submissionId);
          if (waiter) {
            if (msg.submissionDisposition !== "Retryable") {
              await clearDraftDeckSubmission(this.hostPeerId, msg.submissionId);
            }
            waiter.reject(new Error(msg.reason));
          }
        }
        this.emit({ type: "error", message: msg.reason });
        break;
      }

      case "draft_kicked": {
        this.terminated = true;
        this.resolveLeaveAcknowledgement(session);
        await this.revokeRecovery();
        this.failDeckSubmissionWaiters(msg.reason);
        this.emit({ type: "kicked", reason: msg.reason });
        break;
      }

      case "draft_pairing": {
        this.emit({
          type: "pairing",
          round: msg.round,
          table: msg.table,
          opponentName: msg.opponentName,
          matchHostPeerId: msg.matchHostPeerId,
          matchId: msg.matchId,
        });
        break;
      }

      case "draft_match_result": {
        this.emit({
          type: "matchResult",
          matchId: msg.matchId,
          winnerSeat: msg.winnerSeat,
        });
        break;
      }

      case "draft_match_settlement_ack": {
        this.emit({
          type: "matchSettlementAcknowledged",
          matchId: msg.matchId,
          receiptId: msg.receiptId,
          revision: msg.revision,
        });
        break;
      }

      case "draft_paused": {
        this.emit({ type: "draftPaused", reason: msg.reason });
        break;
      }

      case "draft_resumed": {
        this.emit({ type: "draftResumed" });
        break;
      }

      case "draft_lobby_update": {
        this.emit({
          type: "lobbyUpdate",
          seats: msg.seats,
          joined: msg.joined,
          total: msg.total,
        });
        break;
      }

      case "draft_timer_sync": {
        this.emit({ type: "timerSync", remainingMs: msg.remainingMs });
        break;
      }

      case "draft_match_start": {
        this.emit({
          type: "matchStart",
          launch: msg.launch,
        });
        break;
      }

      case "draft_host_left": {
        this.terminated = true;
        this.resolveLeaveAcknowledgement(session);
        await this.revokeRecovery();
        this.failDeckSubmissionWaiters(msg.reason);
        this.emit({ type: "hostLeft", reason: msg.reason });
        break;
      }

      case "draft_bo3_sideboard_prompt": {
        this.emit({
          type: "bo3SideboardPrompt",
          matchId: msg.matchId,
          gameNumber: msg.gameNumber,
          score: msg.score,
          loserSeat: msg.loserSeat,
          timerMs: msg.timerMs,
        });
        break;
      }

      case "draft_bo3_intergame_authorized": {
        this.emit({
          type: "bo3AuthorizedCommand",
          command: msg.command,
          acknowledgement: msg.acknowledgement,
        });
        break;
      }

      case "draft_bo3_play_draw_prompt": {
        this.emit({
          type: "bo3ChoosePlayDraw",
          matchId: msg.matchId,
          gameNumber: msg.gameNumber,
          score: msg.score,
          timerMs: msg.timerMs,
        });
        break;
      }

      case "draft_bo3_game_start": {
        this.emit({
          type: "bo3GameStart",
          matchId: msg.matchId,
          gameNumber: msg.gameNumber,
          firstPlayerSeat: msg.firstPlayerSeat,
        });
        break;
      }

      case "draft_bo3_score_update": {
        this.emit({
          type: "bo3ScoreUpdate",
          matchId: msg.matchId,
          scoreA: msg.scoreA,
          scoreB: msg.scoreB,
        });
        break;
      }

      default:
        break;
    }
  }

  // ── Disconnect / Reconnect ─────────────────────────────────────────

  private handleHostDisconnect(): void {
    if (this.terminated || this.reconnecting || this.leaveAcknowledgement || !this.draftToken) return;
    this.reconnecting = true;
    void this.attemptReconnect(0);
  }

  private async attemptReconnect(attemptIndex: number): Promise<void> {
    if (this.terminated || !this.draftToken) return;

    this.emit({ type: "reconnecting", attempt: attemptIndex + 1 });
    await this.waitForRetry(attemptIndex);

    if (this.terminated || !this.draftToken) return;

    try {
      const conn = await this.openReconnectConnection();
      await this.handshakeOn(conn, undefined, true);
      this.reconnecting = false;
    } catch (err) {
      console.warn(`[P2PDraftGuest] reconnect attempt ${attemptIndex + 1} failed:`, err);
      void this.attemptReconnect(attemptIndex + 1);
    }
  }

  // ── Cleanup ────────────────────────────────────────────────────────

  dispose(): void {
    this.terminated = true;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.failDeckSubmissionWaiters("Draft connection disposed");
    if (this.handshake) this.rejectHandshake(this.handshake.session, abortError());
    if (this.session) {
      this.retireSession(this.session);
      this.session = null;
    }
    this.currentView = null;
    this.listeners = [];
  }

  leave(): Promise<void> {
    if (this.pendingLeave) return this.pendingLeave;
    const leave = this.leaveInner();
    this.pendingLeave = leave;
    void leave.finally(() => {
      if (this.pendingLeave === leave) this.pendingLeave = null;
    }).catch(() => undefined);
    return leave;
  }

  private async leaveInner(): Promise<void> {
    const session = this.session;
    const draftToken = this.draftToken;
    if (!session || !draftToken) throw new Error("Draft leave requires an active session");

    const acknowledgement = new Promise<void>((resolve, reject) => {
      this.leaveAcknowledgement = { session, draftToken, resolve, reject };
    });
    const timeout = setTimeout(() => {
      if (this.leaveAcknowledgement?.session === session) {
        this.leaveAcknowledgement.reject(new Error("Draft host did not acknowledge leave"));
        this.leaveAcknowledgement = null;
      }
    }, LEAVE_ACK_TIMEOUT_MS);

    try {
      // Disconnect can reject the acknowledgement before encoding completes.
      await Promise.all([
        session.send({
          type: "draft_leave",
          draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
          draftToken,
        }),
        acknowledgement,
      ]);
    } finally {
      clearTimeout(timeout);
      if (this.leaveAcknowledgement?.session === session) this.leaveAcknowledgement = null;
    }

    this.terminated = true;
    await this.revokeRecovery();
    this.dispose();
    try {
      this.guestPeer.destroy();
    } catch { /* best-effort */ }
  }

  private resolveLeaveAcknowledgement(session: DraftPeerSession, draftToken?: string): void {
    const pending = this.leaveAcknowledgement;
    if (
      pending
      && pending.session === session
      && (!draftToken || pending.draftToken === draftToken)
    ) {
      this.leaveAcknowledgement = null;
      pending.resolve();
    }
  }

  // ── Accessors ──────────────────────────────────────────────────────

  get view(): DraftPlayerView | null {
    return this.currentView;
  }

  get seat(): number | null {
    return this.seatIndex;
  }

  get token(): string | null {
    return this.draftToken;
  }

  /** Whether the host has durably revoked this guest's reconnect capability. */
  get isRecoveryRevoked(): boolean {
    return this.recoveryRevoked;
  }

  private async revokeRecovery(): Promise<void> {
    await Promise.allSettled([
      clearDraftGuestRecovery(this.hostPeerId),
      clearDraftDeckSubmission(this.hostPeerId),
    ]);
    this.recoveryRevoked = true;
  }

  private async persistRecoveryIdentity(data: { draftToken: string; seatIndex: number; draftCode: string }): Promise<void> {
    await saveDraftGuestSession(this.hostPeerId, {
      ...data,
      roomCode: this.connection.roomCode,
      displayName: this.connection.displayName,
    });
    saveActiveDraftGuest({
      roomCode: this.connection.roomCode,
      displayName: this.connection.displayName,
      hostPeerId: this.hostPeerId,
    });
  }

  private waitForRetry(attemptIndex: number, signal?: AbortSignal): Promise<void> {
    const delay = attemptIndex < RECONNECT_BACKOFF_MS.length
      ? RECONNECT_BACKOFF_MS[attemptIndex]
      : RECONNECT_STEADY_STATE_MS;
    return new Promise((resolve, reject) => {
      let timer: ReturnType<typeof setTimeout> | null = null;
      const onAbort = () => {
        if (timer) clearTimeout(timer);
        this.retryTimer = null;
        reject(abortError());
      };
      signal?.addEventListener("abort", onAbort, { once: true });
      timer = setTimeout(() => {
        this.retryTimer = null;
        signal?.removeEventListener("abort", onAbort);
        resolve();
      }, delay);
      this.retryTimer = timer;
    });
  }

  private openReconnectConnection(signal?: AbortSignal): Promise<DataConnection> {
    if (signal?.aborted) return Promise.reject(abortError());
    const conn = this.guestPeer.connect(this.hostPeerId);
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => finish(() => reject(new Error("connect timed out"))), FIRST_CONTACT_TIMEOUT_MS);
      const onAbort = () => finish(() => reject(abortError()));
      const onOpen = () => finish(() => resolve(conn));
      const onError = (err: Error) => finish(() => reject(err));
      const finish = (complete: () => void) => {
        clearTimeout(timeout);
        signal?.removeEventListener("abort", onAbort);
        complete();
      };
      signal?.addEventListener("abort", onAbort, { once: true });
      conn.on("open", onOpen);
      conn.on("error", onError);
    });
  }
}

function asError(value: unknown): Error {
  return value instanceof Error ? value : new Error(String(value));
}

function abortError(): Error {
  return new DOMException("Draft reconnect aborted", "AbortError");
}
