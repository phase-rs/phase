/**
 * Draft Pod Host Adapter.
 *
 * Lifecycle wrapper that creates a PeerJS peer, optionally registers with
 * the lobby broker, instantiates a `P2PDraftHost`, and exposes a clean
 * event-driven interface for the Zustand `multiplayerDraftStore`.
 *
 * Mirrors the pattern of `P2PHostAdapter` (game host), but the underlying
 * coordinator speaks the `DraftP2PMessage` protocol and manages an 8-seat
 * draft pod instead of a 2-4 player game.
 */

import type { DraftPlayerView, PairingView, PodPolicy, SeatPublicView, TournamentFormat } from "./draft-adapter";
import type { MatchScore } from "./types";
import { P2PDraftHost, type DraftHostEvent } from "./p2p-draft-host";
import { hostRoom, type HostResult } from "../network/connection";
import type { BrokerClient, RegisterHostRequest } from "../services/brokerClient";
import { loadDraftHostSession } from "../services/draftPersistence";

// ── Types ──────────────────────────────────────────────────────────────

export type DraftPodHostStatus =
  | "idle"
  | "connecting"
  | "lobby"
  | "drafting"
  | "deckbuilding"
  | "pairing"
  | "matchInProgress"
  | "roundComplete"
  | "complete"
  | "error";

export type DraftPodHostEvent =
  | { type: "statusChanged"; status: DraftPodHostStatus }
  | { type: "roomCreated"; roomCode: string }
  | { type: "viewUpdated"; view: DraftPlayerView }
  | { type: "lobbyUpdate"; seats: SeatPublicView[]; joined: number; total: number }
  | { type: "lobbyFull" }
  | { type: "draftStarted"; view: DraftPlayerView }
  | { type: "pickReceived"; seatIndex: number; cardInstanceId: string }
  | { type: "roundComplete" }
  | { type: "draftComplete" }
  | { type: "deckSubmitted"; seatIndex: number }
  | { type: "allDecksSubmitted" }
  | { type: "seatJoined"; seatIndex: number; displayName: string }
  | { type: "seatReconnected"; seatIndex: number }
  | { type: "seatDisconnected"; seatIndex: number }
  | { type: "seatKicked"; seatIndex: number; reason: string }
  | { type: "pairingsGenerated"; round: number; pairings: PairingView[] }
  | { type: "matchResultReceived"; matchId: string; winnerSeat: number | null }
  | { type: "roundAdvanced"; newRound: number }
  | { type: "timerExpired" }
  | { type: "bo3SideboardPromptSent"; matchId: string }
  | { type: "bo3BothSideboardsSubmitted"; matchId: string }
  | { type: "bo3GameStarted"; matchId: string; gameNumber: number }
  | { type: "error"; message: string };

type DraftPodHostEventListener = (event: DraftPodHostEvent) => void;

export interface DraftPodHostConfig {
  setPoolJson: string;
  kind: "Premier" | "Traditional";
  podSize: number;
  hostDisplayName: string;
  /** Swiss (3 rounds) or Single Elimination bracket. */
  tournamentFormat: TournamentFormat;
  /** Competitive (timed) or Casual (untimed, host-controlled). */
  podPolicy: PodPolicy;
  /** Broker client for lobby registration. Optional: P2P works without broker. */
  broker?: BrokerClient;
  /** Broker request for lobby registration. Required if broker is set. */
  brokerRequest?: RegisterHostRequest;
  /** Persistence ID for host crash recovery. */
  persistenceId?: string;
  /** Resume from a specific room code (re-hosts on the same PeerJS ID). */
  preferredRoomCode?: string;
  /** Abort signal for cancellation during setup. */
  signal?: AbortSignal;
}

// ── DraftPodHostAdapter ────────────────────────────────────────────────

export class DraftPodHostAdapter {
  private listeners: DraftPodHostEventListener[] = [];
  private host: P2PDraftHost | null = null;
  private hostResult: HostResult | null = null;
  private hostEventUnsub: (() => void) | null = null;
  private _status: DraftPodHostStatus = "idle";
  private _roomCode: string | null = null;

  onEvent(listener: DraftPodHostEventListener): () => void {
    this.listeners.push(listener);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener);
    };
  }

  private emit(event: DraftPodHostEvent): void {
    for (const listener of this.listeners) {
      listener(event);
    }
  }

  private setStatus(status: DraftPodHostStatus): void {
    this._status = status;
    this.emit({ type: "statusChanged", status });
  }

  get status(): DraftPodHostStatus {
    return this._status;
  }

  get roomCode(): string | null {
    return this._roomCode;
  }

  // ── Initialization ─────────────────────────────────────────────────

  /**
   * Create PeerJS peer, optionally register with broker, and start
   * accepting guest connections.
   */
  async initialize(config: DraftPodHostConfig): Promise<void> {
    this.setStatus("connecting");

    try {
      // 1. Create PeerJS host peer
      const hostResult = await hostRoom(config.signal, {
        preferredRoomCode: config.preferredRoomCode,
      });
      this.hostResult = hostResult;
      this._roomCode = hostResult.roomCode;
      this.emit({ type: "roomCreated", roomCode: hostResult.roomCode });

      // 2. Register with lobby broker if provided
      if (config.broker && config.brokerRequest) {
        try {
          await config.broker.registerHost({
            ...config.brokerRequest,
            hostPeerId: hostResult.peerId,
          });
        } catch (err) {
          console.warn("[DraftPodHostAdapter] broker registration failed:", err);
          // Non-fatal: direct room code still works
        }
      }

      // 3. Create P2PDraftHost
      const host = new P2PDraftHost(
        hostResult.peer,
        hostResult.onGuestConnected,
        config.setPoolJson,
        config.kind,
        config.podSize,
        config.hostDisplayName,
        undefined, // default grace period
        config.persistenceId,
        hostResult.roomCode,
      );

      // 4. Wire host events
      this.hostEventUnsub = host.onEvent((event) => {
        this.handleHostEvent(event);
      });

      // 5. Check for persisted session to restore
      if (config.persistenceId) {
        const persisted = await loadDraftHostSession(config.persistenceId);
        if (persisted) {
          const view = await host.restoreFromPersisted(persisted);
          if (view) {
            this.setStatus(
              view.status === "Deckbuilding" ? "deckbuilding" : "drafting",
            );
            this.emit({ type: "viewUpdated", view });
          }
        }
      }

      // 6. Start accepting connections
      await host.initialize();
      this.host = host;

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

  // ── Host event mapping ─────────────────────────────────────────────

  private handleHostEvent(event: DraftHostEvent): void {
    switch (event.type) {
      case "seatJoined":
        this.emit({
          type: "seatJoined",
          seatIndex: event.seatIndex,
          displayName: event.displayName,
        });
        break;
      case "seatReconnected":
        this.emit({ type: "seatReconnected", seatIndex: event.seatIndex });
        break;
      case "seatDisconnected":
        this.emit({ type: "seatDisconnected", seatIndex: event.seatIndex });
        break;
      case "seatKicked":
        this.emit({
          type: "seatKicked",
          seatIndex: event.seatIndex,
          reason: event.reason,
        });
        break;
      case "lobbyUpdate":
        this.emit({
          type: "lobbyUpdate",
          seats: [],
          joined: event.joined,
          total: event.total,
        });
        break;
      case "lobbyFull":
        this.emit({ type: "lobbyFull" });
        break;
      case "draftStarted":
        this.setStatus("drafting");
        this.emit({ type: "draftStarted", view: event.view });
        break;
      case "pickReceived":
        this.emit({
          type: "pickReceived",
          seatIndex: event.seatIndex,
          cardInstanceId: event.cardInstanceId,
        });
        break;
      case "roundComplete":
        this.emit({ type: "roundComplete" });
        break;
      case "draftComplete":
        this.setStatus("deckbuilding");
        this.emit({ type: "draftComplete" });
        break;
      case "deckSubmitted":
        this.emit({ type: "deckSubmitted", seatIndex: event.seatIndex });
        break;
      case "allDecksSubmitted":
        this.setStatus("pairing");
        this.emit({ type: "allDecksSubmitted" });
        break;
      case "error":
        this.emit({ type: "error", message: event.message });
        break;
      case "viewUpdated":
        this.emit({ type: "viewUpdated", view: event.view });
        break;
      case "pairingsGenerated":
        this.setStatus("matchInProgress");
        this.emit({ type: "pairingsGenerated", round: event.round, pairings: event.pairings });
        break;
      case "matchResultReceived":
        this.emit({ type: "matchResultReceived", matchId: event.matchId, winnerSeat: event.winnerSeat });
        break;
      case "roundAdvanced":
        this.setStatus("pairing");
        this.emit({ type: "roundAdvanced", newRound: event.newRound });
        break;
      case "timerExpired":
        this.emit({ type: "timerExpired" });
        break;
      case "bo3SideboardPromptSent":
        this.emit({ type: "bo3SideboardPromptSent", matchId: event.matchId });
        break;
      case "bo3BothSideboardsSubmitted":
        this.emit({ type: "bo3BothSideboardsSubmitted", matchId: event.matchId });
        break;
      case "bo3GameStarted":
        this.emit({ type: "bo3GameStarted", matchId: event.matchId, gameNumber: event.gameNumber });
        break;
    }
  }

  // ── Draft actions ──────────────────────────────────────────────────

  async startDraft(): Promise<void> {
    if (!this.host) throw new Error("Host not initialized");
    await this.host.startDraft();
  }

  async submitPick(cardInstanceId: string): Promise<DraftPlayerView> {
    if (!this.host) throw new Error("Host not initialized");
    return this.host.submitHostPick(cardInstanceId);
  }

  async submitDeck(mainDeck: string[]): Promise<DraftPlayerView> {
    if (!this.host) throw new Error("Host not initialized");
    return this.host.submitHostDeck(mainDeck);
  }

  async getHostView(): Promise<DraftPlayerView> {
    if (!this.host) throw new Error("Host not initialized");
    return this.host.getHostView();
  }

  // ── Match coordination ──────────────────────────────────────────────

  async generatePairings(round: number): Promise<void> {
    if (!this.host) throw new Error("Host not initialized");
    await this.host.generatePairings(round);
  }

  async advanceRound(): Promise<void> {
    if (!this.host) throw new Error("Host not initialized");
    await this.host.advanceRound();
  }

  async overrideMatchResult(matchId: string, winnerSeat: number | null): Promise<void> {
    if (!this.host) throw new Error("Host not initialized");
    await this.host.overrideMatchResult(matchId, winnerSeat);
  }

  async replaceSeatWithBot(seat: number): Promise<void> {
    if (!this.host) throw new Error("Host not initialized");
    await this.host.replaceSeatWithBot(seat);
  }

  // ── Bo3 Between-Games forwarding ───────────────────────────────────

  handleMatchBetweenGames(
    matchId: string,
    gameNumber: number,
    score: MatchScore,
    loserSeat: number | null,
    seatA: number,
    seatB: number,
  ): void {
    if (!this.host) throw new Error("Host not initialized");
    this.host.handleMatchBetweenGames(matchId, gameNumber, score, loserSeat, seatA, seatB);
  }

  handleSideboardSubmit(
    seat: number,
    matchId: string,
    mainDeck: string[],
    sideboard: Array<{ name: string; count: number }>,
  ): void {
    if (!this.host) throw new Error("Host not initialized");
    this.host.handleSideboardSubmit(seat, matchId, mainDeck, sideboard);
  }

  handlePlayDrawChosen(seat: number, matchId: string, playFirst: boolean): void {
    if (!this.host) throw new Error("Host not initialized");
    this.host.handlePlayDrawChosen(seat, matchId, playFirst);
  }

  // ── Host controls ──────────────────────────────────────────────────

  kickPlayer(seat: number, reason?: string): void {
    if (!this.host) throw new Error("Host not initialized");
    this.host.kickPlayer(seat, reason);
  }

  requestPause(): void {
    if (!this.host) throw new Error("Host not initialized");
    this.host.requestPause();
  }

  requestResume(): void {
    if (!this.host) throw new Error("Host not initialized");
    this.host.requestResume();
  }

  get isFull(): boolean {
    return this.host?.isFull ?? false;
  }

  get isStarted(): boolean {
    return this.host?.isStarted ?? false;
  }

  get isPaused(): boolean {
    return this.host?.isPaused ?? false;
  }

  // ── Cleanup ────────────────────────────────────────────────────────

  async dispose(): Promise<void> {
    if (this.hostEventUnsub) {
      this.hostEventUnsub();
      this.hostEventUnsub = null;
    }
    if (this.host) {
      await this.host.terminateDraft();
      this.host = null;
    }
    if (this.hostResult) {
      this.hostResult.destroy();
      this.hostResult = null;
    }
    this.listeners = [];
    this._roomCode = null;
    this.setStatus("idle");
  }
}
