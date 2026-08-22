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

import { DraftAdapter, EMPTY_DRAFT_POOL_GROUPS } from "./draft-adapter";
import type { DraftPlayerView, MultiplayerSeatDescriptor, PairingView, PoolInput, SeatPublicView } from "./draft-adapter";
import type { PodPolicy, TournamentFormat } from "./draft-adapter";
import {
  createDraftPeerSession,
  type DraftPeerSession,
} from "../network/draftPeerSession";
import { DRAFT_PROTOCOL_VERSION, DraftPauseReason } from "../network/draftProtocol";
import type {
  DraftDeckPayload,
  DraftMatchBinding,
  DraftMatchDeckPayload,
  DraftMatchLaunch,
  DraftMatchSettlement,
  DraftP2PMessage,
} from "../network/draftProtocol";
import type { DeckCardCount, MatchConfig, MatchScore } from "./types";
import {
  saveDraftHostSession,
  clearDraftHostSession,
  type PersistedDraftHostSession,
} from "../services/draftPersistence";

function matchConfigForView(view: DraftPlayerView): MatchConfig {
  return view.match_config;
}
import {
  commandAcknowledgement,
  draftIntergameDigest,
  IntergameCommandController,
  matchesCommandAcknowledgement,
  type DraftIntergameCommand,
  type DraftIntergameCommandAck,
} from "../services/intergameCommandLedger";
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
  decks: Array<{ seat: number; main: DeckCardCount[]; sideboard: DeckCardCount[] }>;
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
  | { type: "roundAdvanced" }
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
  | { type: "bo3GameStarted"; matchId: string; gameNumber: number }
  | { type: "bo3AuthorizedCommand"; command: DraftIntergameCommand; acknowledgement: DraftIntergameCommandAck };

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

function deckCardCounts(cards: readonly string[]): DeckCardCount[] {
  const counts = new Map<string, number>();
  for (const card of cards) counts.set(card, (counts.get(card) ?? 0) + 1);
  return [...counts].map(([name, count]) => ({ name, count }));
}

function deckSubmission(deck: DraftDeckPayload): { main: DeckCardCount[]; sideboard: DeckCardCount[] } {
  return {
    main: deckCardCounts(deck.main_deck),
    sideboard: deckCardCounts(deck.sideboard),
  };
}

/** Sideboarding may move cards between zones, but cannot change a player's pool. */
function preservesDeckPool(
  deck: DraftDeckPayload,
  main: readonly DeckCardCount[],
  sideboard: readonly DeckCardCount[],
): boolean {
  const submitted = new Map<string, number>();
  for (const card of [...main, ...sideboard]) {
    if (!Number.isSafeInteger(card.count) || card.count < 0) return false;
    submitted.set(card.name, (submitted.get(card.name) ?? 0) + card.count);
  }
  const original = new Map<string, number>();
  for (const name of [...deck.main_deck, ...deck.sideboard]) {
    original.set(name, (original.get(name) ?? 0) + 1);
  }
  return submitted.size === original.size
    && [...submitted].every(([name, count]) => original.get(name) === count);
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
  /** Registered decks are captured at match launch and become the first
   * authority-owned default for an unchanged sideboard submission. */
  private matchDecks = new Map<string, Map<number, DraftDeckPayload>>();
  /** Full launch records let the host mint a timeout command under the same
   * immutable launch digest the participant originally received. */
  private matchLaunches = new Map<string, Map<number, DraftMatchLaunch>>();
  /** Private issuer for the durable Pending → Authorized → Executing → Receipted ledger. */
  private intergameCommands = new IntergameCommandController();
  private launchDigests = new Map<string, Map<number, string>>();
  /** Durable pod-issued authority records, keyed by match ID. */
  private matchBindings = new Map<string, DraftMatchBinding>();
  /** Write-ahead settlement records; retained until the reducer accepts them. */
  private settlementOutbox = new Map<string, DraftMatchSettlement>();
  /** Immutable receipt per match makes retries idempotent. */
  private settlementReceipts = new Map<string, { receiptId: string; revision: number }>();

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
    private readonly kind: "Premier" | "Traditional" | "Sealed",
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
        if (!this.canGuestPick(seat)) return;
        await this.handlePick(seat, msg.cardInstanceId);
        break;
      }
      case "draft_pick_with_draft_effect": {
        if (!this.canGuestPick(seat)) return;
        await this.handlePickWithDraftEffect(
          seat,
          msg.effectCardInstanceId,
          msg.cardInstanceIds,
        );
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
        // A raw match ID is forgeable by any connected seat. Keep the legacy
        // shape decodable for an in-flight old client, but never settle it.
        this.guestSessions.get(seat)?.send({ type: "draft_error", reason: "Unbound match result" });
        break;
      }
      case "draft_match_settlement": {
        await this.acceptMatchSettlement(seat, msg.settlement);
        break;
      }
      case "draft_bo3_between_games": {
        await this.handleGuestBetweenGames(seat, msg);
        break;
      }
      case "draft_request_advance": {
        // T-57-07: ignore from guests — only host UI triggers round advance
        break;
      }
      case "draft_bo3_sideboard_submit": {
        this.guestSessions.get(seat)?.send({ type: "draft_error", reason: "Unbound intergame command" });
        break;
      }
      case "draft_bo3_play_draw_choice": {
        this.guestSessions.get(seat)?.send({ type: "draft_error", reason: "Unbound intergame command" });
        break;
      }
      case "draft_bo3_intergame_command": {
        this.holdIntergameCommand(seat, msg.command);
        break;
      }
      case "draft_bo3_intergame_receipt": {
        this.receiptIntergameCommand(seat, msg.acknowledgement, msg.receiptId);
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
    const startView = await this.adapter.getViewForSeat(0);
    if (startView.status === "Drafting") {
      await this.resolveBotPicks({ emit: false, persist: false });
    }

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
    if (freshHostView.status === "Drafting") {
      this.startPickTimer(0);
    }
  }

  /**
   * Host submits their own pick (seat 0).
   */
  async submitHostPick(cardInstanceId: string): Promise<DraftPlayerView> {
    return this.handlePick(0, cardInstanceId);
  }

  /** Host submits an effect pick for seat 0. */
  async submitHostPickWithDraftEffect(
    effectCardInstanceId: string,
    cardInstanceIds: string[],
  ): Promise<DraftPlayerView> {
    return this.handlePickWithDraftEffect(0, effectCardInstanceId, cardInstanceIds);
  }

  /**
   * Host submits their own deck (seat 0).
   */
  async submitHostDeck(mainDeck: string[]): Promise<DraftPlayerView> {
    return this.handleDeckSubmission(0, mainDeck);
  }

  private assertPickAllowed(): void {
    if (!this.draftStarted) throw new Error("Draft not started");
    if (this.paused) throw new Error("Draft is paused");
  }

  private canGuestPick(seat: number): boolean {
    try {
      this.assertPickAllowed();
      return true;
    } catch (err) {
      const reason = err instanceof Error ? err.message : String(err);
      this.guestSessions.get(seat)?.send({ type: "draft_error", reason });
      return false;
    }
  }

  private async handlePick(
    seat: number,
    cardInstanceId: string,
    resolveBots = true,
  ): Promise<DraftPlayerView> {
    this.assertPickAllowed();
    return this.applyPick(seat, cardInstanceId, {
      acknowledge: true,
      emit: true,
      persist: true,
      resolveBots,
    });
  }

  private async handlePickWithDraftEffect(
    seat: number,
    effectCardInstanceId: string,
    cardInstanceIds: string[],
  ): Promise<DraftPlayerView> {
    this.assertPickAllowed();
    return this.applyPick(
      seat,
      effectCardInstanceId,
      {
        acknowledge: true,
        emit: true,
        persist: true,
        resolveBots: true,
      },
      () => this.adapter.submitPickWithDraftEffectForSeat(
        seat,
        effectCardInstanceId,
        cardInstanceIds,
      ),
    );
  }

  private async applyPick(
    seat: number,
    cardInstanceId: string,
    options: PickOptions,
    submitPick = () => this.adapter.submitPickForSeat(seat, cardInstanceId),
  ): Promise<DraftPlayerView> {
    try {
      const view = await submitPick();
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
        await this.generatePairings();
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
        this.autoChoosePlayDraw(matchId);
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
   * Generate the next round's pairings and dispatch match start messages.
   * The engine decides which round that is; we read it back off the view.
   * Called after all decks are submitted or after round advancement.
   */
  async generatePairings(): Promise<void> {
    try {
      const view = await this.adapter.generatePairings();
      // The engine owns the round. Read it back; never compute it here.
      const round = view.current_round;
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
      console.error(`[P2PDraftHost] generatePairings failed:`, message);
      this.emit({ type: "error", message: `Failed to generate pairings: ${message}` });
    }
  }

  private matchBindingFor(pairing: PairingView): DraftMatchBinding {
    const existing = this.matchBindings.get(pairing.match_id);
    if (existing && existing.round === pairing.round) return existing;

    const binding: DraftMatchBinding = {
      podId: this.draftCode,
      matchId: pairing.match_id,
      round: pairing.round,
      sessionKey: crypto.randomUUID(),
      lease: crypto.randomUUID(),
      nonce: crypto.randomUUID(),
      revision: 0,
      matchAuthoritySeat: Math.min(pairing.seat_a, pairing.seat_b),
    };
    this.matchBindings.set(pairing.match_id, binding);
    this.persistSession();
    return binding;
  }

  private async acceptMatchSettlement(
    submittingSeat: number,
    settlement: DraftMatchSettlement,
  ): Promise<void> {
    const binding = this.matchBindings.get(settlement.binding.matchId);
    if (!binding || !this.sameBinding(binding, settlement.binding)) {
      this.guestSessions.get(submittingSeat)?.send({ type: "draft_error", reason: "Invalid match binding" });
      return;
    }
    const view = await this.adapter.getViewForSeat(0);
    const pairing = view.pairings.find(
      (candidate) => candidate.match_id === binding.matchId && candidate.round === binding.round,
    );
    if (
      !pairing ||
      view.current_round !== binding.round ||
      submittingSeat !== binding.matchAuthoritySeat ||
      (settlement.winnerSeat !== null &&
        settlement.winnerSeat !== pairing.seat_a &&
        settlement.winnerSeat !== pairing.seat_b)
    ) {
      this.guestSessions.get(submittingSeat)?.send({ type: "draft_error", reason: "Unauthorized match settlement" });
      return;
    }

    const receipt = this.settlementReceipts.get(binding.matchId);
    if (receipt) {
      if (receipt.receiptId === settlement.receiptId) {
        void this.sendSettlementAck(submittingSeat, binding.matchId, receipt);
      } else {
        this.sendToSeat(submittingSeat, {
          type: "draft_error",
          reason: "Match already settled",
        });
      }
      return;
    }

    // Persist the intent before invoking the draft reducer. A recovered pod
    // can retry this record without applying a second result.
    this.settlementOutbox.set(settlement.receiptId, settlement);
    this.persistSession();
    await this.reportMatchResult(binding.matchId, settlement.winnerSeat);
    const accepted = { receiptId: settlement.receiptId, revision: binding.revision };
    this.settlementReceipts.set(binding.matchId, accepted);
    this.settlementOutbox.delete(settlement.receiptId);
    this.persistSession();
    void this.sendSettlementAck(submittingSeat, binding.matchId, accepted);
  }

  private sameBinding(left: DraftMatchBinding, right: DraftMatchBinding): boolean {
    return left.podId === right.podId
      && left.matchId === right.matchId
      && left.round === right.round
      && left.sessionKey === right.sessionKey
      && left.lease === right.lease
      && left.nonce === right.nonce
      && left.revision === right.revision
      && left.matchAuthoritySeat === right.matchAuthoritySeat;
  }

  private async sendSettlementAck(
    seat: number,
    matchId: string,
    receipt: { receiptId: string; revision: number },
  ): Promise<void> {
    const message: DraftP2PMessage = {
      type: "draft_match_settlement_ack",
      matchId,
      receiptId: receipt.receiptId,
      revision: receipt.revision,
    };
    if (seat === 0) return;
    await this.guestSessions.get(seat)?.send(message);
  }

  private async dispatchMatchLaunch(pairing: PairingView, view: DraftPlayerView): Promise<void> {
    const seatA = pairing.seat_a;
    const seatB = pairing.seat_b;
    const seatAIsBot = this.isBotSeatFromView(view, seatA);
    const seatBIsBot = this.isBotSeatFromView(view, seatB);
    const session = await this.exportDraftSession();
    const binding = this.matchBindingFor(pairing);

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

      this.sendMatchLaunch(humanSeat, {
          type: "Bot",
          matchId: pairing.match_id,
          round: pairing.round,
          localSeat: humanSeat,
          botSeat,
          botName,
          deckPayload,
          matchConfig: matchConfigForView(view),
          binding,
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

    this.sendMatchLaunch(matchHostSeat, {
        type: "HumanHost",
        matchId: pairing.match_id,
        matchRoomCode,
        round: pairing.round,
        localSeat: matchHostSeat,
        opponentSeat: guestSeat,
        opponentName: hostOpponentName,
        matchHostPeerId: matchRoomCode,
        deckPayload,
        matchConfig: matchConfigForView(view),
        binding,
    });
    this.sendMatchLaunch(guestSeat, {
        type: "HumanGuest",
        matchId: pairing.match_id,
        matchRoomCode,
        round: pairing.round,
        localSeat: guestSeat,
        opponentSeat: matchHostSeat,
        opponentName: guestOpponentName,
        matchHostPeerId: matchRoomCode,
        localDeck: guestDeck,
        matchConfig: matchConfigForView(view),
        binding,
    });
  }

  private sendMatchLaunch(seat: number, launch: DraftMatchLaunch): void {
    this.rememberMatchDecks(launch);
    let launches = this.matchLaunches.get(launch.matchId);
    if (!launches) {
      launches = new Map();
      this.matchLaunches.set(launch.matchId, launches);
    }
    launches.set(seat, launch);
    let digests = this.launchDigests.get(launch.matchId);
    if (!digests) {
      digests = new Map();
      this.launchDigests.set(launch.matchId, digests);
    }
    digests.set(seat, draftIntergameDigest(launch));
    this.persistSession();
    this.sendToSeat(seat, { type: "draft_match_start", launch });
  }

  private rememberMatchDecks(launch: DraftMatchLaunch): void {
    let decks = this.matchDecks.get(launch.matchId);
    if (!decks) {
      decks = new Map();
      this.matchDecks.set(launch.matchId, decks);
    }
    switch (launch.type) {
      case "HumanHost":
        decks.set(launch.localSeat, launch.deckPayload.player);
        decks.set(launch.opponentSeat, launch.deckPayload.opponent);
        break;
      case "HumanGuest":
        decks.set(launch.localSeat, launch.localDeck);
        break;
      case "Bot":
        decks.set(launch.localSeat, launch.deckPayload.player);
        decks.set(launch.botSeat, launch.deckPayload.opponent);
        break;
    }
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
      throw err;
    }
  }

  /** Seat 0 uses the same authenticated settlement gate as remote match hosts. */
  async submitHostMatchSettlement(settlement: DraftMatchSettlement): Promise<void> {
    await this.acceptMatchSettlement(0, settlement);
  }

  /**
   * Advance to the next round (Casual mode, host-only).
   * T-57-07: only callable from host UI; guests sending draft_request_advance are ignored.
   */
  async advanceRound(): Promise<void> {
    try {
      await this.adapter.advanceRound();
      this.emit({ type: "roundAdvanced" });
      await this.generatePairings();
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
    const decks = this.matchDecks.get(matchId);
    this.bo3State.set(matchId, {
      seatA, seatB,
      submittedA: false, submittedB: false,
      loserSeat, gameNumber, score,
      decks: [seatA, seatB].flatMap((seat) => {
        const deck = decks?.get(seat);
        return deck ? [{ seat, ...deckSubmission(deck) }] : [];
      }),
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

  private async handleGuestBetweenGames(
    seat: number,
    message: Extract<DraftP2PMessage, { type: "draft_bo3_between_games" }>,
  ): Promise<void> {
    const binding = this.matchBindings.get(message.matchId);
    const view = await this.adapter.getViewForSeat(0);
    const pairing = view.pairings.find(
      (candidate) => candidate.match_id === message.matchId && candidate.round === binding?.round,
    );
    if (
      !binding
      || binding.matchAuthoritySeat !== seat
      || view.current_round !== binding.round
      || !pairing
      || (pairing.status !== "Pending" && pairing.status !== "InProgress")
    ) {
      this.guestSessions.get(seat)?.send({ type: "draft_error", reason: "Unauthorized between-games report" });
      return;
    }
    if (this.bo3State.get(message.matchId)?.gameNumber === message.gameNumber) return;

    this.handleMatchBetweenGames(
      message.matchId,
      message.gameNumber,
      message.score,
      message.loserSeat,
      pairing.seat_a,
      pairing.seat_b,
    );
  }

  /** The sole command ingress for host UI and authenticated guest sessions. */
  submitAuthorized(seat: number, command: DraftIntergameCommand): void {
    if (command.status === "Receipted" && command.receiptId) {
      this.receiptIntergameCommand(seat, commandAcknowledgement(command), command.receiptId);
      return;
    }
    this.holdIntergameCommand(seat, command);
  }

  private holdIntergameCommand(seat: number, command: DraftIntergameCommand): void {
    const state = this.bo3State.get(command.matchId);
    const launchDigest = this.launchDigests.get(command.matchId)?.get(seat);
    if (!state
      || command.status !== "Pending"
      || command.seat !== seat
      || command.gameNumber !== state.gameNumber
      || !launchDigest
      || command.launchDigest !== launchDigest
      || draftIntergameDigest(command.launchPayload) !== command.launchDigest
      || command.payloadDigest !== draftIntergameDigest(command.payload)
      || this.intergameCommands.snapshot().some((candidate) => candidate.commandId === command.commandId)) {
      return;
    }
    if (command.payload.type === "SubmitSideboard") {
      const deck = this.matchDecks.get(command.matchId)?.get(seat);
      if (
        (seat !== state.seatA && seat !== state.seatB)
        || !deck
        || !preservesDeckPool(deck, command.payload.main, command.payload.sideboard)
      ) {
        this.sendToSeat(seat, { type: "draft_error", reason: "Invalid sideboard submission" });
        return;
      }
    } else if (state.loserSeat !== seat) {
      return;
    }

    const held = this.intergameCommands.hold({
      commandId: command.commandId,
      matchId: command.matchId,
      gameNumber: command.gameNumber,
      seat,
      payload: command.payload,
      launchPayload: command.launchPayload,
      launchDigest: command.launchDigest,
    });
    this.persistSession();

    switch (held.payload.type) {
      case "SubmitSideboard":
        if (seat === state.seatA) state.submittedA = true;
        else state.submittedB = true;
        if (state.submittedA && state.submittedB) {
          this.clearActiveTimer();
          for (const pending of this.intergameCommands.snapshot()) {
            if (pending.matchId === held.matchId
              && pending.gameNumber === held.gameNumber
              && pending.status === "Pending"
              && pending.payload.type === "SubmitSideboard") {
              this.authorizeIntergameCommand(pending);
            }
          }
          this.emit({ type: "bo3BothSideboardsSubmitted", matchId: held.matchId });
        }
        break;
      case "ChoosePlayDraw":
        this.authorizeIntergameCommand(held);
        break;
    }
  }

  private authorizeIntergameCommand(command: DraftIntergameCommand): void {
    const acknowledgement = commandAcknowledgement(command);
    const authorized = this.intergameCommands.authorize(command.commandId, acknowledgement);
    if (!authorized) return;
    const permit = this.intergameCommands.begin(command.commandId, acknowledgement);
    if (!permit) return;
    // The controller, not a caller supplied flag, owns the Executing state.
    // This deliberate no-op consumption proves the issuer created the permit;
    // the participant performs the same pre-execution check with its own issuer.
    void permit;
    this.persistSession();
    this.sendToSeat(command.seat, {
      type: "draft_bo3_intergame_authorized",
      command: authorized,
      acknowledgement,
    });
  }

  private receiptIntergameCommand(
    seat: number,
    acknowledgement: DraftIntergameCommandAck,
    receiptId: string,
  ): void {
    const command = this.intergameCommands.snapshot().find(
      (candidate) => candidate.commandId === acknowledgement.commandId,
    );
    if (!command || command.seat !== seat || !matchesCommandAcknowledgement(command, acknowledgement)) return;
    const receipted = this.intergameCommands.receipt(command.commandId, acknowledgement, receiptId);
    if (!receipted) return;
    this.persistSession();
    switch (receipted.payload.type) {
      case "SubmitSideboard": {
        const state = this.bo3State.get(receipted.matchId);
        const deck = state?.decks.find((candidate) => candidate.seat === seat);
        if (deck) {
          deck.main = receipted.payload.main;
          deck.sideboard = receipted.payload.sideboard;
        }
        const complete = state && [state.seatA, state.seatB].every((participant) =>
          this.intergameCommands.snapshot().some((candidate) =>
            candidate.matchId === receipted.matchId
              && candidate.gameNumber === receipted.gameNumber
              && candidate.seat === participant
              && candidate.payload.type === "SubmitSideboard"
              && candidate.status === "Receipted"),
        );
        if (complete && state) this.transitionToPlayDraw(receipted.matchId, state);
        break;
      }
      case "ChoosePlayDraw":
        this.resolvePlayDrawChoice(receipted.matchId, receipted.payload.playFirst);
        break;
    }
  }

  private autoSubmitSideboards(matchId: string): void {
    const state = this.bo3State.get(matchId);
    if (!state) return;
    const participants = [state.seatA, state.seatB];
    const submitted = new Set([
      ...(state.submittedA ? [state.seatA] : []),
      ...(state.submittedB ? [state.seatB] : []),
    ]);
    for (const seat of participants) {
      if (submitted.has(seat)) continue;
      const deck = state.decks.find((candidate) => candidate.seat === seat);
      if (!deck) {
        this.emit({ type: "error", message: "Sideboard timer expired without a registered deck" });
        continue;
      }
      this.submitDefaultIntergameCommand(matchId, state, seat, {
        type: "SubmitSideboard",
        main: deck.main,
        sideboard: deck.sideboard,
      });
    }
  }

  private autoChoosePlayDraw(matchId: string): void {
    const state = this.bo3State.get(matchId);
    if (!state || state.loserSeat === null) return;
    this.submitDefaultIntergameCommand(matchId, state, state.loserSeat, {
      type: "ChoosePlayDraw",
      playFirst: true,
    });
  }

  /** Timeout defaults enter the same signed launch/ledger path as a player
   * submission, so they cannot bypass authorization or the execution receipt. */
  private submitDefaultIntergameCommand(
    matchId: string,
    state: Bo3MatchState,
    seat: number,
    payload: DraftIntergameCommand["payload"],
  ): void {
    const launch = this.matchLaunches.get(matchId)?.get(seat);
    const launchDigest = this.launchDigests.get(matchId)?.get(seat);
    if (!launch || !launchDigest) {
      this.emit({ type: "error", message: "Intergame timeout lacks launch authority" });
      return;
    }
    this.holdIntergameCommand(seat, {
      commandId: crypto.randomUUID(),
      matchId,
      gameNumber: state.gameNumber,
      seat,
      payload,
      launchPayload: launch,
      launchDigest,
      payloadDigest: draftIntergameDigest(payload),
      status: "Pending",
    });
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
        case "draft_bo3_intergame_authorized":
          this.emit({
            type: "bo3AuthorizedCommand",
            command: msg.command,
            acknowledgement: msg.acknowledgement,
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
          matchBindings: [...this.matchBindings.values()],
          settlementOutbox: [...this.settlementOutbox.values()],
          settlementReceipts: [...this.settlementReceipts.entries()].map(
            ([matchId, receipt]) => ({ matchId, ...receipt }),
          ),
          intergameCommands: this.intergameCommands.snapshot(),
          bo3State: [...this.bo3State.entries()].map(([matchId, state]) => ({ matchId, ...state })),
          launchDigests: [...this.launchDigests.entries()].flatMap(([matchId, digests]) =>
            [...digests.entries()].map(([seat, digest]) => ({ matchId, seat, digest })),
          ),
          matchLaunches: [...this.matchLaunches.entries()].flatMap(([matchId, launches]) =>
            [...launches.entries()].map(([seat, launch]) => ({ matchId, seat, launch })),
          ),
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
      const params = new URLSearchParams({ host_peer_id: this.hostPeer.id });
      await fetch(
        `${this.backupEndpoint}/p2p-draft-backup/${this.draftCode}?${params}`,
        { method: "DELETE" },
      );
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
    for (const binding of session.matchBindings ?? []) {
      this.matchBindings.set(binding.matchId, binding);
    }
    for (const settlement of session.settlementOutbox ?? []) {
      this.settlementOutbox.set(settlement.receiptId, settlement);
    }
    for (const receipt of session.settlementReceipts ?? []) {
      this.settlementReceipts.set(receipt.matchId, {
        receiptId: receipt.receiptId,
        revision: receipt.revision,
      });
    }
    this.intergameCommands = new IntergameCommandController(session.intergameCommands ?? []);
    this.intergameCommands.recover();
    for (const state of session.bo3State ?? []) {
      const { matchId, ...rest } = state;
      this.bo3State.set(matchId, { ...rest, decks: rest.decks ?? [] });
    }
    for (const { matchId, seat, digest } of session.launchDigests ?? []) {
      let digests = this.launchDigests.get(matchId);
      if (!digests) {
        digests = new Map();
        this.launchDigests.set(matchId, digests);
      }
      digests.set(seat, digest);
    }
    for (const { matchId, seat, launch } of session.matchLaunches ?? []) {
      const binding = launch.binding ?? this.matchBindings.get(matchId);
      if (!binding || binding.matchId !== matchId) continue;
      const recoveredLaunch = launch.binding ? launch : { ...launch, binding } as DraftMatchLaunch;
      let launches = this.matchLaunches.get(matchId);
      if (!launches) {
        launches = new Map();
        this.matchLaunches.set(matchId, launches);
      }
      launches.set(seat, recoveredLaunch);
      this.rememberMatchDecks(recoveredLaunch);
    }

    if (session.draftSessionJson) {
      const view = await this.adapter.importSession(session.draftSessionJson, 2);
      await this.recoverSettlementOutbox(view);

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
        await this.generatePairings();
        return this.adapter.getViewForSeat(0);
      }

      return view;
    }

    return null;
  }

  /** Replays only write-ahead settlements that the restored draft still lacks. */
  private async recoverSettlementOutbox(view: DraftPlayerView): Promise<void> {
    for (const settlement of [...this.settlementOutbox.values()]) {
      const binding = this.matchBindings.get(settlement.binding.matchId);
      const pairing = view.pairings.find(
        (candidate) => candidate.match_id === settlement.binding.matchId,
      );
      if (!binding || !this.sameBinding(binding, settlement.binding) || !pairing) continue;
      if (pairing.status === "Pending" || pairing.status === "InProgress") {
        await this.reportMatchResult(settlement.binding.matchId, settlement.winnerSeat);
      }
      this.settlementReceipts.set(settlement.binding.matchId, {
        receiptId: settlement.receiptId,
        revision: settlement.binding.revision,
      });
      this.settlementOutbox.delete(settlement.receiptId);
    }
    this.persistSession();
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
    this.matchDecks.clear();
    this.matchLaunches.clear();
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
        face_up_draft_cards: [],
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
      draft_effects: [],
      pool_groups: EMPTY_DRAFT_POOL_GROUPS,
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
      match_config: { match_type: this.kind === "Traditional" ? "Bo3" : "Bo1" },
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
