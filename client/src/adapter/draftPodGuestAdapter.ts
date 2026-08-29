/**
 * Draft Pod Guest Adapter.
 *
 * Lifecycle wrapper that joins a draft pod via PeerJS room code,
 * instantiates a `P2PDraftGuest`, and exposes a clean event-driven
 * interface for the Zustand `multiplayerDraftStore`.
 *
 * Mirrors the pattern of `P2PGuestAdapter` (game guest), but the
 * underlying client speaks the `DraftP2PMessage` protocol and
 * participates in an 8-seat draft pod instead of a game.
 */

import type { DraftPlayerView, SeatPublicView } from "./draft-adapter";
import {
  P2PDraftGuest,
  type DraftGuestConnection,
  type DraftGuestEvent,
  type DraftGuestRecoveryFailure,
} from "./p2p-draft-guest";
import type { DraftMatchLaunch, DraftMatchSettlement, DraftPauseReason } from "../network/draftProtocol";
import type { DraftIntergameCommand, DraftIntergameCommandAck } from "../services/intergameCommandLedger";
import { joinRoom, type JoinResult } from "../network/connection";
import type { DraftWorkspaceState } from "../components/draft/workspace/types";

// ── Types ──────────────────────────────────────────────────────────────

export type DraftPodGuestStatus =
  | "idle"
  | "connecting"
  | "lobby"
  | "drafting"
  | "deckbuilding"
  | "matchInProgress"
  | "complete"
  | "kicked"
  | "hostLeft"
  | "error";

export type DraftPodGuestEvent =
  | { type: "statusChanged"; status: DraftPodGuestStatus }
  | { type: "joined"; seatIndex: number; draftCode: string }
  | { type: "reconnected"; seatIndex: number }
  | { type: "workspaceRestored"; workspaceState: DraftWorkspaceState | null }
  | { type: "viewUpdated"; view: DraftPlayerView }
  | { type: "pickAcknowledged"; view: DraftPlayerView }
  | { type: "deckSubmissionAcknowledged"; submissionId: string; view: DraftPlayerView }
  | { type: "lobbyUpdate"; seats: SeatPublicView[]; joined: number; total: number }
  | { type: "draftPaused"; reason: DraftPauseReason }
  | { type: "draftResumed" }
  | {
      type: "pairing";
      round: number;
      table: number;
      opponentName: string;
      matchHostPeerId: string;
      matchId: string;
    }
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

type DraftPodGuestEventListener = (event: DraftPodGuestEvent) => void;

const INITIAL_RECONNECT_JOIN_ATTEMPTS = 3;
const INITIAL_RECONNECT_JOIN_BACKOFF_MS = [500, 1_000] as const;

class HostIdentityMismatchError extends Error {
  constructor() {
    super("Draft pod host changed; reconnect credentials were not sent");
    this.name = "HostIdentityMismatchError";
  }
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

interface DraftPodGuestConnectionBase {
  roomCode: string;
  displayName: string;
  /** Abort signal for cancellation during connection. */
  signal?: AbortSignal;
  /** Connection timeout in ms (default 30s). */
  timeoutMs?: number;
}

/** New joins and credentialed reconnects are intentionally disjoint. */
export type DraftPodGuestConfig =
  | ({ kind: "new" } & DraftPodGuestConnectionBase)
  | ({ kind: "reconnect"; hostPeerId: string; draftToken: string } & DraftPodGuestConnectionBase);

// ── DraftPodGuestAdapter ───────────────────────────────────────────────

export class DraftPodGuestAdapter {
  private listeners: DraftPodGuestEventListener[] = [];
  private guest: P2PDraftGuest | null = null;
  private joinResult: JoinResult | null = null;
  private guestEventUnsub: (() => void) | null = null;
  private _status: DraftPodGuestStatus = "idle";
  private _seatIndex: number | null = null;
  private _draftCode: string | null = null;
  private _currentView: DraftPlayerView | null = null;
  private recoveryFailure: DraftGuestRecoveryFailure | null = null;

  onEvent(listener: DraftPodGuestEventListener): () => void {
    this.listeners.push(listener);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener);
    };
  }

  private emit(event: DraftPodGuestEvent): void {
    for (const listener of this.listeners) {
      listener(event);
    }
  }

  private setStatus(status: DraftPodGuestStatus): void {
    this._status = status;
    this.emit({ type: "statusChanged", status });
  }

  get status(): DraftPodGuestStatus {
    return this._status;
  }

  get seatIndex(): number | null {
    return this._seatIndex;
  }

  get draftCode(): string | null {
    return this._draftCode;
  }

  get currentView(): DraftPlayerView | null {
    return this._currentView;
  }

  // ── Initialization ─────────────────────────────────────────────────

  /** Connect to a draft pod host and wait for its first authoritative ack. */
  async initialize(config: DraftPodGuestConfig): Promise<void> {
    this.setStatus("connecting");

    try {
      // 1. Join the PeerJS room
      const { joinResult, reconnectAttemptLimit } = await this.openRoom(config);
      this.joinResult = joinResult;

      // The code route and the IndexedDB capability are both bound to this
      // exact host. A different PeerJS target must never receive the token.
      if (config.kind === "reconnect" && joinResult.conn.peer !== config.hostPeerId) {
        joinResult.destroyPeer();
        this.joinResult = null;
        throw new HostIdentityMismatchError();
      }

      const connection: DraftGuestConnection = config.kind === "new"
        ? { kind: "new", roomCode: config.roomCode, displayName: config.displayName }
        : {
          kind: "reconnect",
          roomCode: config.roomCode,
          displayName: config.displayName,
          draftToken: config.draftToken,
        };

      // 2. Create P2PDraftGuest
      const guest = new P2PDraftGuest(
        joinResult.peer,
        joinResult.conn.peer,
        joinResult.conn,
        connection,
      );

      // 3. Wire guest events
      this.guestEventUnsub = guest.onEvent((event) => {
        this.handleGuestEvent(event);
      });

      // 4. Initialize only resolves after welcome/reconnect acknowledgement.
      // Retain ownership before awaiting so supersession can abort an
      // acknowledgement wait without revoking the persisted capability.
      this.guest = guest;
      if (reconnectAttemptLimit === undefined) {
        await guest.initialize(config.signal);
      } else {
        await guest.initialize(config.signal, reconnectAttemptLimit);
      }

      if (this._status === "connecting") {
        this.setStatus("lobby");
      }
    } catch (err) {
      if (isAbortError(err)) throw err;
      this.setStatus("error");
      const message = err instanceof Error ? err.message : String(err);
      if (config.kind === "reconnect" && !this.recoveryFailure) {
        const failure: DraftGuestRecoveryFailure = err instanceof HostIdentityMismatchError
          ? { kind: "invalid", message }
          : { kind: "retryable", message };
        this.recoveryFailure = failure;
        this.emit({ type: "reconnectFailed", failure });
      }
      this.emit({ type: "error", message });
      throw err;
    }
  }

  /** A fresh seat never retries; a persisted reconnect capability gets a small, abortable join budget. */
  private async openRoom(
    config: DraftPodGuestConfig,
  ): Promise<{ joinResult: JoinResult; reconnectAttemptLimit?: number }> {
    if (config.kind === "new") {
      return { joinResult: await joinRoom(config.roomCode, config.signal, config.timeoutMs) };
    }

    let lastError: unknown;
    for (let attempt = 0; attempt < INITIAL_RECONNECT_JOIN_ATTEMPTS; attempt++) {
      if (config.signal?.aborted) throw abortError();
      try {
        return {
          joinResult: await joinRoom(config.roomCode, config.signal, config.timeoutMs),
          // The initial room join consumes the same bounded recovery budget as
          // a later acknowledgement retry; the first handshake always gets one
          // attempt after a successful room connection.
          reconnectAttemptLimit: Math.max(1, INITIAL_RECONNECT_JOIN_ATTEMPTS - attempt),
        };
      } catch (error) {
        lastError = error;
        if (config.signal?.aborted || attempt === INITIAL_RECONNECT_JOIN_ATTEMPTS - 1) break;
        await waitForReconnectJoinBackoff(INITIAL_RECONNECT_JOIN_BACKOFF_MS[attempt]!, config.signal);
      }
    }
    throw lastError instanceof Error ? lastError : new Error(String(lastError));
  }

  // ── Guest event mapping ────────────────────────────────────────────

  private handleGuestEvent(event: DraftGuestEvent): void {
    switch (event.type) {
      case "joined":
        this._seatIndex = event.seatIndex;
        this._draftCode = event.draftCode;
        this.setStatus("lobby");
        this.emit({
          type: "joined",
          seatIndex: event.seatIndex,
          draftCode: event.draftCode,
        });
        break;
      case "reconnected":
        this._seatIndex = event.seatIndex;
        this.emit({ type: "reconnected", seatIndex: event.seatIndex });
        break;
      case "workspaceRestored":
        this.emit({ type: "workspaceRestored", workspaceState: event.workspaceState });
        break;
      case "viewUpdated":
        this._currentView = event.view;
        this.updateStatusFromView(event.view);
        this.emit({ type: "viewUpdated", view: event.view });
        break;
      case "pickAcknowledged":
        this._currentView = event.view;
        this.emit({ type: "pickAcknowledged", view: event.view });
        break;
      case "deckSubmissionAcknowledged":
        this._currentView = event.view;
        this.emit({
          type: "deckSubmissionAcknowledged",
          submissionId: event.submissionId,
          view: event.view,
        });
        break;
      case "lobbyUpdate":
        this.emit({
          type: "lobbyUpdate",
          seats: event.seats,
          joined: event.joined,
          total: event.total,
        });
        break;
      case "draftPaused":
        this.emit({ type: "draftPaused", reason: event.reason });
        break;
      case "draftResumed":
        this.emit({ type: "draftResumed" });
        break;
      case "pairing":
        this.emit({
          type: "pairing",
          round: event.round,
          table: event.table,
          opponentName: event.opponentName,
          matchHostPeerId: event.matchHostPeerId,
          matchId: event.matchId,
        });
        break;
      case "matchResult":
        this.emit({
          type: "matchResult",
          matchId: event.matchId,
          winnerSeat: event.winnerSeat,
        });
        break;
      case "matchSettlementAcknowledged":
        this.emit({
          type: "matchSettlementAcknowledged",
          matchId: event.matchId,
          receiptId: event.receiptId,
          revision: event.revision,
        });
        break;
      case "timerSync":
        this.emit({ type: "timerSync", remainingMs: event.remainingMs });
        break;
      case "matchStart":
        this.setStatus("matchInProgress");
        this.emit({
          type: "matchStart",
          launch: event.launch,
        });
        break;
      case "kicked":
        this.setStatus("kicked");
        this.emit({ type: "kicked", reason: event.reason });
        break;
      case "hostLeft":
        this.setStatus("hostLeft");
        this.emit({ type: "hostLeft", reason: event.reason });
        break;
      case "error":
        this.emit({ type: "error", message: event.message });
        break;
      case "reconnecting":
        this.emit({ type: "reconnecting", attempt: event.attempt });
        break;
      case "reconnectFailed":
        this.setStatus("error");
        this.recoveryFailure = event.failure;
        this.emit({ type: "reconnectFailed", failure: event.failure });
        break;
      case "bo3SideboardPrompt":
        this.emit({
          type: "bo3SideboardPrompt",
          matchId: event.matchId,
          gameNumber: event.gameNumber,
          score: event.score,
          loserSeat: event.loserSeat,
          timerMs: event.timerMs,
        });
        break;
      case "bo3ChoosePlayDraw":
        this.emit({
          type: "bo3ChoosePlayDraw",
          matchId: event.matchId,
          gameNumber: event.gameNumber,
          score: event.score,
          timerMs: event.timerMs,
        });
        break;
      case "bo3GameStart":
        this.emit({
          type: "bo3GameStart",
          matchId: event.matchId,
          gameNumber: event.gameNumber,
          firstPlayerSeat: event.firstPlayerSeat,
        });
        break;
      case "bo3AuthorizedCommand":
        this.emit({ type: "bo3AuthorizedCommand", command: event.command, acknowledgement: event.acknowledgement });
        break;
      case "bo3ScoreUpdate":
        this.emit({
          type: "bo3ScoreUpdate",
          matchId: event.matchId,
          scoreA: event.scoreA,
          scoreB: event.scoreB,
        });
        break;
    }
  }

  private updateStatusFromView(view: DraftPlayerView): void {
    switch (view.status) {
      case "Drafting":
        if (this._status !== "drafting") this.setStatus("drafting");
        break;
      case "Deckbuilding":
        if (this._status !== "deckbuilding") this.setStatus("deckbuilding");
        break;
      case "Pairing":
      case "RoundComplete":
        break;
      case "MatchInProgress":
        if (this._status !== "matchInProgress") this.setStatus("matchInProgress");
        break;
      case "Complete":
        if (this._status !== "complete") this.setStatus("complete");
        break;
      case "Lobby":
        if (this._status !== "lobby") this.setStatus("lobby");
        break;
      case "Paused":
      case "Abandoned":
        break;
    }
  }

  // ── Draft actions ──────────────────────────────────────────────────

  async submitPick(cardInstanceIds: string[]): Promise<void> {
    if (!this.guest) throw new Error("Guest not initialized");
    await this.guest.submitPick(cardInstanceIds);
  }

  async submitPickWithDraftEffect(
    effectCardInstanceId: string,
    cardInstanceIds: string[],
  ): Promise<void> {
    if (!this.guest) throw new Error("Guest not initialized");
    await this.guest.submitPickWithDraftEffect(effectCardInstanceId, cardInstanceIds);
  }

  async submitDeck(mainDeck: string[], commanders: string[]): Promise<void> {
    if (!this.guest) throw new Error("Guest not initialized");
    await this.guest.submitDeck(mainDeck, commanders);
  }

  async updateWorkspace(state: DraftWorkspaceState): Promise<void> {
    if (!this.guest) throw new Error("Guest not initialized");
    await this.guest.updateWorkspace(state);
  }

  sendMatchSettlement(settlement: DraftMatchSettlement): void {
    this.guest?.sendMatchSettlement(settlement);
  }

  handleMatchBetweenGames(
    matchId: string,
    gameNumber: number,
    score: { p0_wins: number; p1_wins: number; draws: number },
    loserSeat: number | null,
  ): void {
    this.guest?.sendBetweenGames(matchId, gameNumber, score, loserSeat);
  }

  submitAuthorized(command: DraftIntergameCommand): void {
    this.guest?.sendAuthorizedIntergameCommand(command);
  }

  acknowledgeAuthorized(acknowledgement: DraftIntergameCommandAck, receiptId: string): void {
    this.guest?.sendIntergameReceipt(acknowledgement, receiptId);
  }

  // ── Cleanup ────────────────────────────────────────────────────────

  /**
   * Transport/lifecycle disposal preserves credentials by default. Only an
   * explicit participant leave is allowed to revoke durable guest recovery.
   */
  async dispose({ preserveRecovery = true }: { preserveRecovery?: boolean } = {}): Promise<void> {
    if (this.guest) {
      if (preserveRecovery) {
        this.guest.dispose();
      } else if (this.guest.isRecoveryRevoked) {
        // Terminal host events already removed the capability, so there is
        // no live participant session left to acknowledge another leave.
        this.guest.dispose();
      } else {
        await this.guest.leave();
      }
      this.guest = null;
    }
    if (this.guestEventUnsub) {
      this.guestEventUnsub();
      this.guestEventUnsub = null;
    }
    if (this.joinResult) {
      this.joinResult.destroyPeer();
      this.joinResult = null;
    }
    this.listeners = [];
    this._currentView = null;
    this._seatIndex = null;
    this._draftCode = null;
    this.setStatus("idle");
  }
}

function waitForReconnectJoinBackoff(delayMs: number, signal?: AbortSignal): Promise<void> {
  if (signal?.aborted) return Promise.reject(abortError());
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, delayMs);
    const onAbort = () => {
      clearTimeout(timer);
      reject(abortError());
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

function abortError(): Error {
  return new DOMException("Draft reconnect aborted", "AbortError");
}
