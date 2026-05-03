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
import { DRAFT_PROTOCOL_VERSION } from "../network/draftProtocol";
import type { DraftP2PMessage } from "../network/draftProtocol";
import {
  saveDraftGuestSession,
  clearDraftGuestSession,
} from "../services/draftPersistence";

// ── Types ──────────────────────────────────────────────────────────────

export type DraftGuestEvent =
  | { type: "joined"; seatIndex: number; draftCode: string }
  | { type: "reconnected"; seatIndex: number }
  | { type: "viewUpdated"; view: DraftPlayerView }
  | { type: "pickAcknowledged"; view: DraftPlayerView }
  | { type: "lobbyUpdate"; seats: SeatPublicView[]; joined: number; total: number }
  | { type: "draftPaused"; reason: string }
  | { type: "draftResumed" }
  | { type: "pairing"; round: number; table: number; opponentName: string; matchHostPeerId: string; matchId: string }
  | { type: "matchResult"; matchId: string; winnerSeat: number | null }
  | { type: "timerSync"; remainingMs: number }
  | { type: "matchStart"; matchId: string; round: number; opponentSeat: number; opponentName: string; matchHostPeerId: string; isMatchHost: boolean }
  | { type: "bo3SideboardPrompt"; matchId: string; gameNumber: number; score: { p0_wins: number; p1_wins: number; draws: number }; loserSeat: number | null; timerMs: number }
  | { type: "bo3ChoosePlayDraw"; matchId: string; gameNumber: number; score: { p0_wins: number; p1_wins: number; draws: number }; timerMs: number }
  | { type: "bo3GameStart"; matchId: string; gameNumber: number; firstPlayerSeat: number }
  | { type: "bo3ScoreUpdate"; matchId: string; scoreA: number; scoreB: number }
  | { type: "kicked"; reason: string }
  | { type: "hostLeft"; reason: string }
  | { type: "error"; message: string }
  | { type: "reconnecting"; attempt: number }
  | { type: "reconnectFailed"; reason: string };

type DraftGuestEventListener = (event: DraftGuestEvent) => void;

const RECONNECT_BACKOFF_MS = [1_000, 2_000, 4_000, 8_000, 15_000, 30_000, 60_000];
const RECONNECT_STEADY_STATE_MS = 60_000;

// ── P2PDraftGuest ──────────────────────────────────────────────────────

export class P2PDraftGuest {
  private listeners: DraftGuestEventListener[] = [];
  private session: DraftPeerSession | null = null;
  private draftToken: string | null = null;
  private seatIndex: number | null = null;
  private terminated = false;
  private currentView: DraftPlayerView | null = null;

  constructor(
    private readonly guestPeer: Peer,
    private readonly hostPeerId: string,
    private readonly initialConn: DataConnection,
    private readonly displayName: string,
    existingDraftToken?: string,
  ) {
    if (existingDraftToken) {
      this.draftToken = existingDraftToken;
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

  async initialize(): Promise<void> {
    this.attachSession(this.initialConn);

    if (this.draftToken) {
      this.session!.send({ type: "draft_reconnect", draftToken: this.draftToken });
    } else {
      this.session!.send({ type: "draft_join", displayName: this.displayName });
    }
  }

  private attachSession(conn: DataConnection): void {
    const session = createDraftPeerSession(conn, {
      onSessionEnd: () => {
        this.handleHostDisconnect();
      },
    });
    this.session = session;
    session.onMessage((msg) => this.handleHostMessage(msg));
  }

  // ── Actions ────────────────────────────────────────────────────────

  async submitPick(cardInstanceId: string): Promise<void> {
    if (!this.session) throw new Error("Not connected to draft host");
    await this.session.send({ type: "draft_pick", cardInstanceId });
  }

  async submitDeck(mainDeck: string[]): Promise<void> {
    if (!this.session) throw new Error("Not connected to draft host");
    await this.session.send({ type: "draft_submit_deck", mainDeck });
  }

  sendMatchResult(matchId: string, winnerSeat: number | null): void {
    if (!this.session) return;
    void this.session.send({ type: "draft_match_result", matchId, winnerSeat });
  }

  sendSideboardSubmit(matchId: string, mainDeck: string[], sideboard: Array<{ name: string; count: number }>): void {
    if (!this.session) return;
    void this.session.send({ type: "draft_bo3_sideboard_submit", matchId, mainDeck, sideboard });
  }

  sendPlayDrawChoice(matchId: string, playFirst: boolean): void {
    if (!this.session) return;
    void this.session.send({ type: "draft_bo3_play_draw_choice", matchId, playFirst });
  }

  // ── Message handling ───────────────────────────────────────────────

  private handleHostMessage(msg: DraftP2PMessage): void {
    // Protocol version check on first-contact messages
    if (msg.type === "draft_welcome" || msg.type === "draft_reconnect_ack") {
      if (msg.draftProtocolVersion !== DRAFT_PROTOCOL_VERSION) {
        const reason = `Draft protocol mismatch: host v${msg.draftProtocolVersion}, client v${DRAFT_PROTOCOL_VERSION}. Refresh both windows.`;
        console.error("[P2PDraftGuest]", reason);
        this.terminated = true;
        this.emit({ type: "reconnectFailed", reason });
        return;
      }
    }

    switch (msg.type) {
      case "draft_welcome": {
        this.seatIndex = msg.seatIndex;
        this.draftToken = msg.draftToken;
        this.currentView = msg.view;

        // Persist token at join time (P2P-04)
        void saveDraftGuestSession(this.hostPeerId, {
          draftToken: msg.draftToken,
          seatIndex: msg.seatIndex,
          draftCode: msg.draftCode,
        });

        this.emit({ type: "joined", seatIndex: msg.seatIndex, draftCode: msg.draftCode });
        this.emit({ type: "viewUpdated", view: msg.view });
        break;
      }

      case "draft_reconnect_ack": {
        this.seatIndex = msg.seatIndex;
        this.currentView = msg.view;

        if (this.draftToken) {
          void saveDraftGuestSession(this.hostPeerId, {
            draftToken: this.draftToken,
            seatIndex: msg.seatIndex,
            draftCode: msg.draftCode,
          });
        }

        this.emit({ type: "reconnected", seatIndex: msg.seatIndex });
        this.emit({ type: "viewUpdated", view: msg.view });
        break;
      }

      case "draft_reconnect_rejected": {
        this.terminated = true;
        this.emit({ type: "reconnectFailed", reason: msg.reason });
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

      case "draft_error": {
        this.emit({ type: "error", message: msg.reason });
        break;
      }

      case "draft_kicked": {
        this.terminated = true;
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
          matchId: msg.matchId,
          round: msg.round,
          opponentSeat: msg.opponentSeat,
          opponentName: msg.opponentName,
          matchHostPeerId: msg.matchHostPeerId,
          isMatchHost: msg.isMatchHost,
        });
        break;
      }

      case "draft_host_left": {
        this.terminated = true;
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
    this.session = null;
    if (this.terminated) return;
    void this.attemptReconnect(0);
  }

  private async attemptReconnect(attemptIndex: number): Promise<void> {
    if (this.terminated) return;

    const delay = attemptIndex < RECONNECT_BACKOFF_MS.length
      ? RECONNECT_BACKOFF_MS[attemptIndex]
      : RECONNECT_STEADY_STATE_MS;

    this.emit({ type: "reconnecting", attempt: attemptIndex + 1 });
    await new Promise((r) => setTimeout(r, delay));

    if (this.terminated) return;

    try {
      const conn = this.guestPeer.connect(this.hostPeerId);
      await new Promise<void>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("connect timed out")), 10_000);
        conn.on("open", () => { clearTimeout(timeout); resolve(); });
        conn.on("error", (err) => { clearTimeout(timeout); reject(err); });
      });
      this.attachSession(conn);
      if (this.draftToken) {
        this.session!.send({ type: "draft_reconnect", draftToken: this.draftToken });
      }
    } catch (err) {
      console.warn(`[P2PDraftGuest] reconnect attempt ${attemptIndex + 1} failed:`, err);
      void this.attemptReconnect(attemptIndex + 1);
    }
  }

  // ── Cleanup ────────────────────────────────────────────────────────

  dispose(): void {
    this.terminated = true;
    if (this.session) {
      this.session.close();
      this.session = null;
    }
    this.currentView = null;
    this.listeners = [];
  }

  async leave(): Promise<void> {
    this.terminated = true;
    if (this.draftToken) {
      void clearDraftGuestSession(this.hostPeerId);
    }
    this.dispose();
    try {
      this.guestPeer.destroy();
    } catch { /* best-effort */ }
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
}
