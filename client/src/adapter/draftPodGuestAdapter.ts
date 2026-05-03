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
import { P2PDraftGuest, type DraftGuestEvent } from "./p2p-draft-guest";
import { joinRoom, type JoinResult } from "../network/connection";
import { loadDraftGuestSession } from "../services/draftPersistence";

// ── Types ──────────────────────────────────────────────────────────────

export type DraftPodGuestStatus =
  | "idle"
  | "connecting"
  | "lobby"
  | "drafting"
  | "deckbuilding"
  | "complete"
  | "kicked"
  | "hostLeft"
  | "error";

export type DraftPodGuestEvent =
  | { type: "statusChanged"; status: DraftPodGuestStatus }
  | { type: "joined"; seatIndex: number; draftCode: string }
  | { type: "reconnected"; seatIndex: number }
  | { type: "viewUpdated"; view: DraftPlayerView }
  | { type: "pickAcknowledged"; view: DraftPlayerView }
  | { type: "lobbyUpdate"; seats: SeatPublicView[]; joined: number; total: number }
  | { type: "draftPaused"; reason: string }
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
  | { type: "kicked"; reason: string }
  | { type: "hostLeft"; reason: string }
  | { type: "error"; message: string }
  | { type: "reconnecting"; attempt: number }
  | { type: "reconnectFailed"; reason: string };

type DraftPodGuestEventListener = (event: DraftPodGuestEvent) => void;

export interface DraftPodGuestConfig {
  roomCode: string;
  displayName: string;
  /** Abort signal for cancellation during connection. */
  signal?: AbortSignal;
  /** Connection timeout in ms (default 30s). */
  timeoutMs?: number;
}

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

  /**
   * Connect to a draft pod host via PeerJS room code. Checks for an
   * existing draft token in IndexedDB for reconnection.
   */
  async initialize(config: DraftPodGuestConfig): Promise<void> {
    this.setStatus("connecting");

    try {
      // 1. Join the PeerJS room
      const joinResult = await joinRoom(
        config.roomCode,
        config.signal,
        config.timeoutMs,
      );
      this.joinResult = joinResult;

      // 2. Check for existing draft token (reconnect case)
      const persisted = await loadDraftGuestSession(joinResult.peer.id);
      const existingToken = persisted?.draftToken;

      // 3. Create P2PDraftGuest
      const guest = new P2PDraftGuest(
        joinResult.peer,
        joinResult.conn.peer,
        joinResult.conn,
        config.displayName,
        existingToken,
      );

      // 4. Wire guest events
      this.guestEventUnsub = guest.onEvent((event) => {
        this.handleGuestEvent(event);
      });

      // 5. Initialize (sends join or reconnect message)
      await guest.initialize();
      this.guest = guest;

      if (this._status === "connecting") {
        this.setStatus("lobby");
      }
    } catch (err) {
      this.setStatus("error");
      const message = err instanceof Error ? err.message : String(err);
      this.emit({ type: "error", message });
      throw err;
    }
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
      case "viewUpdated":
        this._currentView = event.view;
        this.updateStatusFromView(event.view);
        this.emit({ type: "viewUpdated", view: event.view });
        break;
      case "pickAcknowledged":
        this._currentView = event.view;
        this.emit({ type: "pickAcknowledged", view: event.view });
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
        this.emit({ type: "reconnectFailed", reason: event.reason });
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
      case "Complete":
        if (this._status !== "complete") this.setStatus("complete");
        break;
    }
  }

  // ── Draft actions ──────────────────────────────────────────────────

  async submitPick(cardInstanceId: string): Promise<void> {
    if (!this.guest) throw new Error("Guest not initialized");
    await this.guest.submitPick(cardInstanceId);
  }

  async submitDeck(mainDeck: string[]): Promise<void> {
    if (!this.guest) throw new Error("Guest not initialized");
    await this.guest.submitDeck(mainDeck);
  }

  // ── Cleanup ────────────────────────────────────────────────────────

  async dispose(): Promise<void> {
    if (this.guestEventUnsub) {
      this.guestEventUnsub();
      this.guestEventUnsub = null;
    }
    if (this.guest) {
      await this.guest.leave();
      this.guest = null;
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
