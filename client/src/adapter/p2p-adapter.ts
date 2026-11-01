import type {
  AiActionProposal,
  AiDecisionDiagnosticReceipt,
  AiProposalSubmission,
  ContestClickInput,
  ContestDriveResult,
  EngineAdapter,
  EngineSnapshot,
  FormatConfig,
  GameAction,
  GameEvent,
  GameLogEntry,
  GameState,
  LegalActionsResult,
  MatchConfig,
  ObjectId,
  PlayerId,
  PersistedGameState,
  RestoredGameStateResult,
  StateWithheldNotice,
  SubmitResult,
  TakebackEvent,
  TakebackNotice,
  TakebackReply,
  ViewerSnapshot,
  WaitingFor,
} from "./types";
import type {
  InteractionPreview,
  InteractionPreviewRequest,
  InteractionSubmission,
} from "./generated/interaction";
import type { BracketDeckRequest, BracketEstimate } from "../types/bracketEstimate";

import {
  AdapterError,
  AdapterErrorCode,
  EMPTY_LEGAL_ACTIONS,
  actionRejectionError,
  isActionRejection,
  nextSnapshotSeq,
} from "./types";
import {
  createHostSessionOwner,
  getHostAdapter,
  type HostSessionOwner,
} from "./wasm-adapter";
import {
  acknowledgeFullTerminalDelivery,
  bootstrapFullTerminalDelivery,
  WebSocketAdapter,
  type NativeAiSeat,
  type NativeSessionAttachment,
} from "./ws-adapter";
import { dialPeer, RECONNECT_DIAL_TIMEOUT_MS } from "../network/connection";
import { ContestClickBuffer, isContestSequence } from "../network/contestClickBuffer";
import { createPeerSession, type PeerSession } from "../network/peer";
import type { TransportConnection, TransportPeer } from "../network/transport";
import type { P2PMessage } from "../network/protocol";
import {
  WIRE_PROTOCOL_VERSION,
  legalActionsFromWire,
  legalActionsToWire,
  takebackEventFromWire,
  takebackEventToWire,
} from "../network/protocol";
import type {
  DeckListData,
  PlayerSlot,
  SeatKind,
  SeatMutation,
  SeatState,
  SeatMutationResult,
  SeatView,
} from "../multiplayer/seatTypes";
import type { BrokerClient } from "../services/brokerClient";
import type { FullSessionKey } from "../services/multiplayerSession";
import type { FullTerminalDelivery } from "../services/fullTerminalResult";
import {
  clearFullTerminalCleanupStrict,
  clearP2PHostSession,
  clearGame,
  clearGameStrict,
  saveFullTerminalCleanupStrict,
  type FullTerminalCleanupObligation,
  type FullTerminalCleanupDelivery,
  type NativeAiDriverFault,
  type NativeP2PServerSession,
  type PersistedP2PHostSession,
  saveGame,
  saveResumableGameStrict,
  saveP2PHostSession,
} from "../services/gamePersistence";
import {
  claimP2PHostLease,
  createP2PSessionKey,
  hasExactP2PAuthority,
  isP2PAuthorityStamp,
  ownsP2PHostLease,
  releaseP2PHostLease,
  clearP2PSession,
  saveP2PSession,
  type P2PAuthorityStamp,
  type P2PSessionKey,
} from "../services/p2pSession";
import {
  commitP2PTerminalResult,
  isValidP2PTerminalResult,
  p2pFinalStateCommitment,
  sameP2PTerminalResult,
  type P2PTerminalResult,
} from "../services/p2pTerminalResult";
import { NativeEngineSocket } from "../services/nativeEngineSocket";
import i18n from "i18next";

/**
 * Adapter-level events emitted to the UI. Wire-protocol messages are
 * snake_case (`player_kicked`); adapter events stay camelCase
 * (`playerKicked`). The adapter performs the remap inside its message
 * handlers — the UI never sees wire types.
 */
export type P2PAdapterEvent =
  | { type: "playerLatencies"; latencies: Record<number, number | null> }
  | { type: "playerIdentity"; playerId: PlayerId; playerNames?: Record<number, string> }
  | { type: "roomCreated"; roomCode: string }
  | { type: "waitingForGuest" }
  | { type: "guestConnected" }
  | { type: "opponentDisconnected"; reason: string }
  | { type: "gameOver"; winner: PlayerId | null; reason: string }
  | { type: "terminalResult"; result: P2PTerminalResult }
  | { type: "terminalUnavailable"; message: string }
  | { type: "error"; message: string }
  | { type: "operationFailed"; message: string }
  /**
   * Pre-game setup failure on the host side. Distinct from the catch-all
   * `error` event because it carries a typed `reason` for the UI to render
   * a specific remediation — not every setup error is the same problem.
   * Currently only `room_still_claimed` fires (PeerJS signaling server
   * still holds the prior host's peer-id registration after a fast
   * resume); future classifications slot in as additional `reason` arms.
   */
  | { type: "hostingFailed"; reason: "room_still_claimed"; message: string }
  | {
      /**
       * The engine pair travels as ONE `EngineSnapshot` rather than separate
       * `state`/`legalResult` fields: the two halves plus their ordering stamp
       * stay inseparable by construction, so no consumer can pair a state from
       * one engine version with legal actions from another.
       */
      type: "stateChanged";
      snapshot: EngineSnapshot;
      events: GameEvent[];
      logEntries?: GameLogEntry[];
    }
  /** The latest state was applied but withheld (`null`: a delivered state ended that). */
  | { type: "stateDelivery"; withheld: StateWithheldNotice | null }
  // 3-4p multiplayer additions:
  | {
      type: "opponentDisconnectedWithChoice";
      playerId: PlayerId;
      gracePeriodMs: number;
    }
  | { type: "playerKicked"; playerId: PlayerId; reason: string }
  | { type: "playerConceded"; playerId: PlayerId; reason: string }
  | { type: "playerReconnected"; playerId: PlayerId }
  | { type: "gamePaused"; reason: string }
  | { type: "gameResumed" }
  | { type: "lobbyProgress"; joined: number; total: number }
  | { type: "playerSlotsUpdated"; slots: PlayerSlot[] }
  | { type: "roomFull" }
  | { type: "deckRejected"; reason: string; format?: string }
  | { type: "reconnecting"; attempt: number }
  | { type: "reconnectFailed"; reason: string }
  /** The authority sent this seat a fresh bootstrap; it replays a still-open takeback vote after this event. */
  | { type: "bootstrapDelivered" }
  | TakebackEvent;

type P2PAdapterEventListener = (event: P2PAdapterEvent) => void;

function reconnectRejectionReason(
  message: Extract<P2PMessage, { type: "reconnect_rejected" }>,
): string {
  switch (message.reasonCode) {
    case "first_message_invalid":
      return i18n.t("multiplayer:reconnectRejected.firstMessageInvalid");
    case "wire_protocol_version_required":
      return i18n.t("multiplayer:reconnectRejected.versionRequired", {
        version: message.hostWireProtocolVersion,
      });
    case "wire_protocol_mismatch":
      return i18n.t("multiplayer:reconnectRejected.versionMismatch", {
        guestVersion: message.guestWireProtocolVersion,
        hostVersion: message.hostWireProtocolVersion,
      });
    case "malformed_authority":
      return i18n.t("multiplayer:reconnectRejected.malformedAuthority");
    case undefined:
      return message.reason;
  }
}

type DeckSeatPayload = DeckListData;

interface DeckListPayload {
  player: DeckSeatPayload;
  opponent: DeckSeatPayload;
  ai_decks: DeckSeatPayload[];
  /** AI difficulty strings per seat. See `DeckList.ai_difficulties` in engine. */
  ai_difficulties?: string[];
  /**
   * Every set whose draft boosters this game's decks were drafted from, carried
   * verbatim from the pod. CR 903.13f(3): a draft that contained Commander
   * Masters boosters grants the partner ability, for deckbuilding purposes, to
   * any card that can be a commander by itself whose color identity is one or
   * fewer colors. A LIST because that rule asks about CONTAINMENT, so a
   * mixed-set draft must carry every set it contained. Matches its sources
   * (`DraftMatchDeckPayload.draft_set_codes`, `DraftPlayerView.draft_set_codes`)
   * and the engine's tolerant deserializer, which reads absent, `null` and `[]`
   * identically as constructed play (no grant).
   */
  draft_set_codes?: string[] | null;
  booster_pack_pool?: string[] | null;
}

/** The desktop host has already ensured this exact local phase-server binary
 * before the lobby is advertised. Guests still connect only through PeerJS. */
export interface NativeP2PHostOptions {
  expectedServerVersion?: string;
}

/** Installed only by a pod-issued draft match binding. */
export interface BoundP2PMatchConcede {
  /** The authenticated game seat that chose to concede the whole match. */
  onConcede(concedingPlayer: PlayerId): void | Promise<void>;
}

type NativeViewerUpdate = {
  snapshot: EngineSnapshot;
  events: GameEvent[];
  logEntries?: GameLogEntry[];
  revision: number;
};

/** One seat's server outcome for a revision: its view, or the notice that replaced an oversized one. */
type NativeSeatReport =
  | { kind: "view"; update: NativeViewerUpdate }
  | { kind: "withheld"; notice: StateWithheldNotice };

function seatReportRevision(report: NativeSeatReport): number {
  return report.kind === "view" ? report.update.revision : report.notice.revision;
}

/** The view a reattached seat is sent, with the engine revision it belongs to. */
type NativeResumedView = { revision: number; snapshot: EngineSnapshot };

/** How long a reattached native seat may take to receive its first view. */
const NATIVE_RESUME_VIEW_TIMEOUT_MS = 10_000;

/** Reject with `onTimeout()` if `promise` has not settled within `ms`. */
function withTimeout<T>(promise: Promise<T>, ms: number, onTimeout: () => Error): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(onTimeout()), ms);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

/**
 * Local-only multiplexor for a native authoritative P2P host. There is one
 * loopback socket for every human P2P seat; the server authenticates each
 * action from that socket while PeerJS remains the guest-facing transport.
 */
class NativeP2PBridge {
  private readonly clients = new Map<PlayerId, WebSocketAdapter>();
  private readonly playerTokens = new Map<PlayerId, string>();
  private readonly latestViews = new Map<PlayerId, NativeViewerUpdate>();
  private readonly terminalDeliveries = new Map<PlayerId, FullTerminalDelivery>();
  private readonly pendingViews = new Map<number, Map<PlayerId, NativeSeatReport>>();
  /** Each seat's latest withheld notice; `latestViews` cannot reach its revision. */
  private readonly withheldNotices = new Map<PlayerId, StateWithheldNotice>();
  private readonly startWaiters: Array<(update: NativeViewerUpdate) => void> = [];
  /** Suspended seats being reattached, each waiting for its first view of the game. */
  private readonly resumeWaiters = new Map<
    PlayerId,
    { promise: Promise<NativeResumedView>; resolve: (view: NativeResumedView) => void }
  >();
  /** Preserve the server's revision order while asynchronous PeerJS frame
   * encoding runs; a terminal commitment must follow its final state frame. */
  private revisionQueue: Promise<void> = Promise.resolve();
  private deliveredRevision = -1;
  private deliveredFaultIds = new Set<number>();
  private readonly pendingFaults = new Map<number, { id: number; revision: number; message: string }>();
  /** Terminal revision of the host's Full delivery, released after that revision's fan-out. */
  private pendingHostTerminalRevision: number | null = null;
  /** Each seat's takeback notices in arrival order, each held until the revision
   * that seat had been sent when it arrived has been fanned out. */
  private pendingSeatNotices: Array<{
    playerId: PlayerId;
    session: object | undefined;
    revision: number;
    event: TakebackEvent;
  }> = [];
  private gameCode: string | null = null;
  private fullKey: FullSessionKey | null = null;
  /** Clients still in their pregame handshake; `dispose()` releases them. */
  private readonly pending = new Set<WebSocketAdapter>();
  private disposed = false;

  constructor(
    private readonly hostDeckData: DeckListPayload,
    private readonly hostDisplayName: string,
    private readonly playerCount: number,
    private readonly formatConfig: FormatConfig | undefined,
    private readonly matchConfig: MatchConfig | undefined,
    private readonly options: NativeP2PHostOptions,
    private readonly onRevision: (revision: number, reports: Map<PlayerId, NativeSeatReport>) => Promise<void>,
    private readonly onFault: (fault: { id: number; revision: number; message: string }) => Promise<void>,
    private readonly onHostTerminalDelivery: (revision: number) => Promise<void>,
    private readonly onSeatNotice: (playerId: PlayerId, event: TakebackEvent) => Promise<void>,
    /** The guest session a seat's notice is addressed to; `undefined` for the host seat. */
    private readonly seatSession: (playerId: PlayerId) => object | undefined,
    private readonly resumeSession?: NativeP2PServerSession,
  ) {}

  async initializeHost(aiSeats: NativeAiSeat[]): Promise<NativeSessionAttachment> {
    if (this.resumeSession) {
      this.gameCode = this.resumeSession.gameCode;
      this.fullKey = this.resumeSession.fullKey;
      for (const [pidText, token] of Object.entries(this.resumeSession.playerTokens)) {
        this.playerTokens.set(Number(pidText), token);
      }
      const hostToken = this.resumeSession.playerTokens[0];
      if (!hostToken) {
        throw new AdapterError("P2P_ERROR", "Native resume is missing the host token", false);
      }
      try {
        const hostAttachment = await this.reconnectClient(0, hostToken);
        for (const [pidText, token] of Object.entries(this.resumeSession.playerTokens)) {
          const playerId = Number(pidText);
          if (playerId === 0) continue;
          await this.reconnectClient(playerId, token);
        }
        return hostAttachment;
      } catch (reconnectError) {
        // A terminal Full session may already have been retired. Only then
        // bootstrap the existing recipient-bound terminal rows; live resumes
        // take the ordinary same-seat reconnect path without a second request.
        if (!(await this.restoreTerminalViews())) throw reconnectError;
        return {
          gameCode: this.gameCode,
          playerId: 0,
          playerToken: hostToken,
          fullKey: this.fullKey,
        };
      }
    }
    const host = new WebSocketAdapter(
      "native-engine://phase-server",
      "host",
      this.hostDeckData.player,
      undefined,
      undefined,
      undefined,
      this.hostDisplayName,
      {
        nativePregame: {
          kind: "host",
          socketFactory: () => new NativeEngineSocket(),
          expectedServerVersion: this.options.expectedServerVersion,
          callerManagedTerminalDelivery: true,
          playerCount: this.playerCount,
          aiSeats,
          formatConfig: this.formatConfig,
          matchConfig: this.matchConfig,
          boosterPackPool: this.hostDeckData.booster_pack_pool,
        },
      },
    );
    const initialSlots = host.waitForPlayerSlots();
    // `dispose()` can reject the slot wait while `attachClient` is still pending.
    void initialSlots.catch(() => {});
    const attachment = await this.attachClient(host);
    await initialSlots;
    if (attachment.playerId !== 0) {
      host.dispose();
      throw new AdapterError("P2P_ERROR", "Native host was assigned a non-host seat", false);
    }
    this.gameCode = attachment.gameCode;
    this.fullKey = attachment.fullKey;
    return attachment;
  }

  async attachGuest(
    p2pPlayerId: PlayerId,
    deck: DeckListPayload["player"],
    displayName: string,
  ): Promise<NativeSessionAttachment> {
    if (!this.gameCode) {
      throw new AdapterError("P2P_ERROR", "Native host session has not been created", false);
    }
    const guest = new WebSocketAdapter(
      "native-engine://phase-server",
      "join",
      deck,
      this.gameCode,
      undefined,
      undefined,
      displayName,
      {
        nativePregame: {
          kind: "guest",
          socketFactory: () => new NativeEngineSocket(),
          expectedServerVersion: this.options.expectedServerVersion,
          callerManagedTerminalDelivery: true,
        },
      },
    );
    const hostSlots = this.clientFor(0).waitForPlayerSlots();
    // `dispose()` can reject the slot wait while `attachClient` is still pending.
    void hostSlots.catch(() => {});
    const attachment = await this.attachClient(guest);
    await hostSlots;
    if (attachment.playerId !== p2pPlayerId) {
      guest.dispose();
      throw new AdapterError(
        "P2P_ERROR",
        `Native seat mismatch: P2P seat ${p2pPlayerId} was assigned server seat ${attachment.playerId}`,
        false,
      );
    }
    return attachment;
  }

  async start(): Promise<SubmitResult> {
    const host = this.clientFor(0);
    const started = new Promise<NativeViewerUpdate>((resolve) => this.startWaiters.push(resolve));
    const waits = [...this.clients.values()].map((client) => client.waitForGameStarted());
    const all = Promise.all(waits);
    // `dispose()` can reject the waits while the Start round trip is still pending.
    void all.catch(() => {});
    await host.sendSeatMutation({ type: "Start" });
    await all;
    const hostUpdate = await started;
    return { events: hostUpdate.events, log_entries: hostUpdate.logEntries };
  }

  async applySeatMutation(mutation: SeatMutation): Promise<void> {
    await this.clientFor(0).sendSeatMutation(mutation);
  }

  async submitAction(action: GameAction, playerId: PlayerId): Promise<SubmitResult> {
    await this.ensureAttached(playerId);
    return this.clientFor(playerId).submitAction(action, playerId);
  }

  async submitInteraction(
    submission: InteractionSubmission,
    playerId: PlayerId,
  ): Promise<SubmitResult> {
    await this.ensureAttached(playerId);
    return this.clientFor(playerId).submitInteraction(submission, playerId);
  }

  /** A suspended seat that must act (the host conceding for it, say) reattaches first:
   *  the engine accepts an action only on the seat's own authenticated socket. */
  private async ensureAttached(playerId: PlayerId): Promise<void> {
    if (this.clients.has(playerId) || !this.playerTokens.has(playerId)) return;
    try {
      await this.reattachGuest(playerId);
    } catch (cause) {
      // Nothing was sent: callers treat this as a definite non-commit and may retry.
      throw new AdapterError(
        AdapterErrorCode.ACTION_NOT_SENT,
        `Native seat ${playerId} could not be reattached: ${cause instanceof Error ? cause.message : String(cause)}`,
        false,
      );
    }
  }

  async previewManaPayment(action: GameAction, playerId: PlayerId): Promise<ObjectId[]> {
    return this.clientFor(playerId).previewManaPayment(action, playerId);
  }

  async exportPersistenceState(): Promise<string> {
    return this.clientFor(0).exportPersistenceState();
  }

  async previewInteraction(
    request: InteractionPreviewRequest,
    playerId: PlayerId,
  ): Promise<InteractionPreview> {
    return this.clientFor(playerId).previewInteraction(request, playerId);
  }

  async getState(): Promise<GameState> {
    return this.clients.get(0)?.getState()
      ?? this.latestViews.get(0)?.snapshot.state
      ?? Promise.reject(new AdapterError("P2P_ERROR", "Native host state is unavailable", true));
  }

  async getLegalActions(): Promise<LegalActionsResult> {
    return this.clients.get(0)?.getLegalActions()
      ?? this.latestViews.get(0)?.snapshot.legalResult
      ?? Promise.reject(new AdapterError("P2P_ERROR", "Native host actions are unavailable", true));
  }

  async getSnapshot(): Promise<EngineSnapshot> {
    return this.clients.get(0)?.getSnapshot()
      ?? this.latestViews.get(0)?.snapshot
      ?? Promise.reject(new AdapterError("P2P_ERROR", "Native host snapshot is unavailable", true));
  }

  viewerSnapshot(playerId: PlayerId): EngineSnapshot {
    const update = this.latestViews.get(playerId);
    if (!update) {
      throw new AdapterError("P2P_ERROR", `No native snapshot for seat ${playerId}`, false);
    }
    return update.snapshot;
  }

  async abandon(): Promise<void> {
    const host = this.clients.get(0);
    if (host) await host.sendAbandonGame();
  }

  /**
   * A guest's PeerJS channel dropped, but the seat is still in the game. Close the
   * seat's loopback socket so the engine authority sees it go away (a contest pauses
   * its clock and starts that seat's reconnect grace), while keeping the seat's token and
   * last view so it can return. Unlike `detachGuest`, which removes the seat for good.
   */
  suspendGuest(playerId: PlayerId): void {
    if (playerId === 0) return;
    const client = this.clients.get(playerId);
    if (!client) return;
    this.clients.delete(playerId);
    client.dispose();
    // The seat can no longer hold back a revision the rest have all reported.
    this.flushCompletedRevisions();
  }

  /** Whether the seat is attached to the engine authority (not suspended or removed). */
  isGuestAttached(playerId: PlayerId): boolean {
    return this.clients.has(playerId);
  }

  /**
   * The seat's view when it is already its recipient-bound Full terminal
   * delivery at that delivery's terminal revision, as `restoreTerminalViews`
   * or `retainSuspendedTerminalViews` leaves it once the Full session has been
   * retired. Such a seat has no engine authority to reattach to. Returns `null`
   * while the seat still holds a live (older) view or its terminal has not been
   * committed: a seat suspended while the game was live stopped receiving
   * views, and is only given its final view once the terminal commit succeeds.
   */
  retainedTerminalView(playerId: PlayerId): NativeResumedView | null {
    const delivery = this.terminalDeliveries.get(playerId);
    const view = this.latestViews.get(playerId);
    if (
      !delivery?.finalView
      || !view
      || view.revision !== delivery.terminalRevision
      || view.snapshot.state.waiting_for.type !== "GameOver"
    ) return null;
    return { revision: view.revision, snapshot: view.snapshot };
  }

  /**
   * The view a returning, suspended seat is handed: its retained terminal view,
   * else the current view of a live reattachment. When the reattachment is
   * refused because the Full session has been retired (a terminal the seat was
   * away for, whose commit has not seeded it yet or did not complete), the
   * recipient-bound terminal rows are restored the way `reconcileConcede` and a
   * native resume do. Only when there is no terminal to restore does the
   * reattachment's own failure stand, leaving the seat suspended.
   */
  async returningSeatView(playerId: PlayerId): Promise<NativeResumedView> {
    const retained = this.retainedTerminalView(playerId);
    if (retained) return retained;
    try {
      return await this.resumeGuest(playerId);
    } catch (resumeError) {
      // A restore that itself fails leaves the reattachment's failure as the
      // seat's outcome: it is what the host reports for the refused return.
      if (!(await this.restoreTerminalViews().catch(() => false))) throw resumeError;
      const restored = this.retainedTerminalView(playerId);
      if (!restored) throw resumeError;
      return restored;
    }
  }

  /**
   * Keep a suspended seat's Full final view once the host's terminal commit has
   * succeeded, so its return is served from it rather than from a loopback
   * attachment the retired session refuses. Called only after the commit:
   * a terminal that did not commit leaves the seat's view untouched.
   */
  retainSuspendedTerminalViews(finalViews: ReadonlyMap<PlayerId, GameState>, revision: number): void {
    for (const [playerId, finalView] of finalViews) {
      if (this.finalViewIsSeatAuthority(playerId, revision)) this.seedTerminalView(playerId, finalView, revision);
    }
  }

  /** No loopback view of `revision` exists to cross-check for a suspended guest or a seat withheld at it. */
  private finalViewIsSeatAuthority(playerId: PlayerId, revision: number): boolean {
    return (playerId !== 0 && !this.clients.has(playerId)) || this.withheldNotices.get(playerId)?.revision === revision;
  }

  /**
   * Reattach a suspended seat to the engine authority on its own token, without waiting
   * for the view it is sent. A no-op for a seat that is attached or unknown.
   */
  async reattachGuest(playerId: PlayerId): Promise<void> {
    if (this.clients.has(playerId)) return;
    const token = this.playerTokens.get(playerId);
    if (token === undefined) {
      throw new AdapterError("P2P_ERROR", `No token to resume native seat ${playerId}`, false);
    }
    await this.reconnectClient(playerId, token);
  }

  /**
   * [`reattachGuest`], then the current view the engine sent the seat on its return,
   * with the revision it belongs to. A failed return leaves the seat suspended.
   */
  async resumeGuest(playerId: PlayerId): Promise<NativeResumedView> {
    const existing = this.resumeWaiters.get(playerId);
    if (existing) return existing.promise;
    let resolve!: (view: NativeResumedView) => void;
    const promise = new Promise<NativeResumedView>((settle) => {
      resolve = settle;
    });
    this.resumeWaiters.set(playerId, { promise, resolve });
    try {
      await this.reattachGuest(playerId);
      return await withTimeout(promise, NATIVE_RESUME_VIEW_TIMEOUT_MS, () =>
        new AdapterError("P2P_ERROR", `Native seat ${playerId} sent no view on resume`, true),
      );
    } catch (error) {
      // A half-attached client must not linger: the seat stays suspended.
      this.suspendGuest(playerId);
      throw error;
    } finally {
      this.resumeWaiters.delete(playerId);
    }
  }

  detachGuest(playerId: PlayerId): void {
    if (playerId === 0) return;
    this.clients.get(playerId)?.dispose();
    this.clients.delete(playerId);
    this.playerTokens.delete(playerId);
    this.latestViews.delete(playerId);
    this.withheldNotices.delete(playerId);
  }

  dispose(): void {
    this.disposed = true;
    for (const client of this.pending) client.dispose();
    for (const client of this.clients.values()) client.dispose();
    this.clients.clear();
    this.playerTokens.clear();
    this.latestViews.clear();
    this.withheldNotices.clear();
    this.pendingViews.clear();
    this.resumeWaiters.clear();
    this.pendingSeatNotices = [];
  }

  private async attachClient(client: WebSocketAdapter): Promise<NativeSessionAttachment> {
    if (this.disposed) {
      // `dispose()` already ran, so it will never release this client.
      client.dispose();
      throw new AdapterError("P2P_ERROR", "Native bridge disposed during attachment", true);
    }
    client.onEvent((event) => {
      if (event.type === "sessionAttached") {
        if (this.disposed) return;
        // Register the exact authenticated seat before GameStarted/reconnect
        // can release a local state frame. This membership is the barrier's
        // recipient set for every following revision.
        this.clients.set(event.attachment.playerId, client);
        this.playerTokens.set(event.attachment.playerId, event.attachment.playerToken);
        return;
      }
      if (event.type === "terminalDelivery") {
        const playerId = client.playerId;
        if (
          playerId !== null
          && this.fullKey
          && event.delivery.key.game_code === this.fullKey.game_code
          && event.delivery.key.generation === this.fullKey.generation
        ) {
          this.terminalDeliveries.set(playerId, event.delivery);
          if (playerId === 0) {
            this.pendingHostTerminalRevision = event.delivery.terminalRevision;
            this.enqueueHostTerminalBarrier();
          }
        }
        return;
      }
      if (event.type === "aiDriverFault") {
        if (this.deliveredFaultIds.has(event.id)) return;
        this.pendingFaults.set(event.id, { id: event.id, revision: event.revision, message: event.message });
        this.enqueueFaultBarrier();
        return;
      }
      if (
        event.type === "takebackRequested"
        || event.type === "takebackResolved"
        || event.type === "requestRejected"
      ) {
        const playerId = client.playerId;
        if (this.disposed || playerId === null) return;
        this.pendingSeatNotices.push({
          playerId,
          session: this.seatSession(playerId),
          revision: this.seatRevision(playerId),
          event,
        });
        this.enqueueSeatNoticeBarrier();
        return;
      }
      if (event.type === "stateDelivery") {
        // The clear arrives as the seat's next delivered view, which already crosses the barrier.
        const playerId = client.playerId;
        if (!event.withheld || playerId === null || event.withheld.revision <= this.deliveredRevision) return;
        this.withheldNotices.set(playerId, event.withheld);
        this.recordSeatReport(playerId, event.withheld.revision, { kind: "withheld", notice: event.withheld });
        return;
      }
      if (event.type !== "stateChanged" || event.serverRevision === undefined) return;
      const revision = event.serverRevision;
      const playerId = client.playerId;
      if (playerId === null) return;
      const update: NativeViewerUpdate = {
        snapshot: event.snapshot,
        events: event.events,
        logEntries: event.logEntries,
        revision,
      };
      this.latestViews.set(playerId, update);
      this.withheldNotices.delete(playerId);
      const resume = this.resumeWaiters.get(playerId);
      if (resume) {
        this.resumeWaiters.delete(playerId);
        resume.resolve({ revision, snapshot: event.snapshot });
        // A returning seat's snapshot is the current state, not a revision the other
        // seats are about to report: it must not wait for them.
        if (revision <= this.deliveredRevision) return;
      }
      this.recordSeatReport(playerId, revision, { kind: "view", update });
    });
    this.pending.add(client);
    const attachment = await client.initializePregame().finally(() => this.pending.delete(client));
    if (this.disposed) {
      throw new AdapterError("P2P_ERROR", "Native bridge disposed during attachment", true);
    }
    this.clients.set(attachment.playerId, client);
    this.playerTokens.set(attachment.playerId, attachment.playerToken);
    return attachment;
  }

  private recordSeatReport(playerId: PlayerId, revision: number, report: NativeSeatReport): void {
    const reports = this.pendingViews.get(revision) ?? new Map<PlayerId, NativeSeatReport>();
    reports.set(playerId, report);
    this.pendingViews.set(revision, reports);
    this.flushCompletedRevisions();
  }

  /** Hand every revision that each currently attached seat has now reported to the
   * fan-out, oldest first. A seat that detached no longer holds a revision back. */
  private flushCompletedRevisions(): void {
    const complete = [...this.pendingViews.entries()]
      .filter(([, reports]) => [...this.clients.keys()].every((seat) => reports.has(seat)))
      .sort(([a], [b]) => a - b);
    for (const [revision, reports] of complete) {
      this.pendingViews.delete(revision);
      this.revisionQueue = this.revisionQueue
        .then(async () => {
          await this.onRevision(revision, reports);
          this.deliveredRevision = Math.max(this.deliveredRevision, revision);
          await this.releaseFaultsThroughDeliveredRevision();
          await this.releaseSeatNoticesThroughDeliveredRevision();
          await this.releaseHostTerminalThroughDeliveredRevision();
        })
        .catch((error) => {
          console.error("[NativeP2PBridge] revision fan-out failed:", error);
        });
      const hostReport = reports.get(0);
      if (hostReport?.kind === "view") {
        for (const resolve of this.startWaiters.splice(0)) resolve(hostReport.update);
      }
    }
  }

  private enqueueFaultBarrier(): void {
    this.revisionQueue = this.revisionQueue
      .then(() => this.releaseFaultsThroughDeliveredRevision())
      .catch((error) => console.error("[NativeP2PBridge] AI fault fan-out failed:", error));
  }

  private async releaseFaultsThroughDeliveredRevision(): Promise<void> {
    for (const fault of [...this.pendingFaults.values()]) {
      if (this.deliveredFaultIds.has(fault.id) || this.deliveredRevision < fault.revision) continue;
      this.pendingFaults.delete(fault.id);
      this.deliveredFaultIds.add(fault.id);
      await this.onFault(fault);
    }
  }

  /** The latest revision the server has sent this seat, as a view or a withheld notice. */
  private seatRevision(playerId: PlayerId): number {
    return Math.max(
      this.latestViews.get(playerId)?.revision ?? -1,
      this.withheldNotices.get(playerId)?.revision ?? -1,
    );
  }

  private enqueueSeatNoticeBarrier(): void {
    this.revisionQueue = this.revisionQueue
      .then(() => this.releaseSeatNoticesThroughDeliveredRevision())
      .catch((error) => console.error("[NativeP2PBridge] takeback notice fan-out failed:", error));
  }

  /** A seat's notice follows the state it was sent before it, as on that seat's socket.
   * A notice dies with its guest session. */
  private async releaseSeatNoticesThroughDeliveredRevision(): Promise<void> {
    const released = this.pendingSeatNotices.filter((notice) => notice.revision <= this.deliveredRevision);
    if (released.length === 0) return;
    this.pendingSeatNotices = this.pendingSeatNotices.filter((notice) => notice.revision > this.deliveredRevision);
    for (const notice of released) {
      if (this.seatSession(notice.playerId) !== notice.session) continue;
      await this.onSeatNotice(notice.playerId, notice.event);
    }
  }

  private enqueueHostTerminalBarrier(): void {
    this.revisionQueue = this.revisionQueue
      .then(() => this.releaseHostTerminalThroughDeliveredRevision())
      .catch((error) => console.error("[NativeP2PBridge] host terminal release failed:", error));
  }

  /** The host's terminal signal follows its revision's fan-out, whichever arrives first. */
  private async releaseHostTerminalThroughDeliveredRevision(): Promise<void> {
    const revision = this.pendingHostTerminalRevision;
    if (revision === null || this.deliveredRevision < revision) return;
    this.pendingHostTerminalRevision = null;
    // A host withheld at its terminal is handed its Full final view here, whatever the later commit's outcome.
    const finalView = this.terminalDeliveries.get(0)?.finalView;
    if (finalView && this.withheldNotices.get(0)?.revision === revision) this.seedTerminalView(0, finalView, revision);
    await this.onHostTerminalDelivery(revision);
  }

  hostTerminalDelivery(): FullTerminalDelivery | null {
    return this.terminalDeliveries.get(0) ?? null;
  }

  persistence(): NativeP2PServerSession | null {
    if (!this.gameCode || !this.fullKey) return null;
    return {
      gameCode: this.gameCode,
      fullKey: this.fullKey,
      playerTokens: Object.fromEntries(this.playerTokens),
    };
  }

  /** Each seat's latest outcome: its notice when that is newer than its last view, else the view. */
  seatReports(): Map<PlayerId, NativeSeatReport> {
    const reports = new Map<PlayerId, NativeSeatReport>();
    for (const [playerId, update] of this.latestViews) reports.set(playerId, { kind: "view", update });
    for (const [playerId, notice] of this.withheldNotices) {
      if (notice.revision > (this.latestViews.get(playerId)?.revision ?? -1)) {
        reports.set(playerId, { kind: "withheld", notice });
      }
    }
    return reports;
  }

  playerIds(): PlayerId[] {
    return [...this.playerTokens.keys()];
  }

  async terminalResume(): Promise<{
    revision: number;
    snapshot: EngineSnapshot;
    display: FullTerminalDelivery["display"];
  } | null> {
    const deliveries = await this.readTerminalDeliveries();
    if (!deliveries) return null;
    const host = this.latestViews.get(0);
    const display = deliveries.get(0)?.display;
    if (!host || !display || host.snapshot.state.waiting_for.type !== "GameOver") return null;
    return { revision: host.revision, snapshot: host.snapshot, display };
  }

  async terminalCleanupAuthority(
    revision: number,
  ): Promise<{ fullKey: FullSessionKey; terminalRevision: number; deliveries: FullTerminalCleanupDelivery[] }> {
    const deliveries = await this.readTerminalDeliveries();
    if (!deliveries || !this.fullKey) {
      throw new Error("Full terminal cleanup authority is unavailable");
    }
    return {
      fullKey: { ...this.fullKey },
      terminalRevision: revision,
      deliveries: [...deliveries].map(([recipient, delivery]) => {
        if (delivery.terminalRevision !== revision || !delivery.finalView) {
          throw new Error("Full terminal cleanup authority is incomplete");
        }
        return {
          recipient,
          deliveryId: delivery.deliveryId,
          credential: delivery.credential,
        };
      }),
    };
  }

  async acknowledgeTerminalCleanup(obligation: FullTerminalCleanupObligation): Promise<void> {
    if (
      !this.fullKey
      || obligation.fullKey.game_code !== this.fullKey.game_code
      || obligation.fullKey.generation !== this.fullKey.generation
      || obligation.terminalRevision < 0
    ) {
      throw new Error("Full terminal cleanup obligation does not match this session");
    }
    for (const delivery of obligation.deliveries) {
      if (!this.playerTokens.has(delivery.recipient)) {
        throw new Error("Full terminal cleanup recipient is no longer bound to this session");
      }
      const cached = this.terminalDeliveries.get(delivery.recipient);
      if (
        cached
        && (
          cached.key.game_code !== obligation.fullKey.game_code
          || cached.key.generation !== obligation.fullKey.generation
          || cached.terminalRevision !== obligation.terminalRevision
          || cached.deliveryId !== delivery.deliveryId
          || cached.credential !== delivery.credential
        )
      ) {
        throw new Error("Full terminal cleanup recipient binding changed");
      }
      if (!(await acknowledgeFullTerminalDelivery(
        "native-engine://phase-server",
        delivery.deliveryId,
        delivery.credential,
        () => new NativeEngineSocket(),
      ))) {
        throw new Error("Full terminal delivery acknowledgement was rejected");
      }
    }
  }

  async terminalFinalViews(revision: number): Promise<Map<PlayerId, GameState>> {
    const deliveries = await this.readTerminalDeliveries();
    if (!deliveries) throw new AdapterError("P2P_ERROR", "Full terminal authority is unavailable", true);
    const states = new Map<PlayerId, GameState>();
    for (const [playerId, delivery] of deliveries) {
      // A guest seat suspended while the game was live (`suspendGuest`), or any
      // seat whose state frame at this revision was withheld, has no loopback
      // view of the terminal revision to cross-check. Its recipient-bound
      // Full final view is the only authority for it: `readTerminalDeliveries`
      // already bound it to this terminal revision, display and Full key. A
      // guest's view is retained (`retainSuspendedTerminalViews`) only once the
      // caller's commit succeeds. Every other seat keeps the strict check below.
      if (
        this.finalViewIsSeatAuthority(playerId, revision)
        && delivery.terminalRevision === revision
        && delivery.finalView?.waiting_for.type === "GameOver"
      ) {
        states.set(playerId, delivery.finalView);
        continue;
      }
      const current = this.latestViews.get(playerId);
      if (
        delivery.terminalRevision !== revision
        || !delivery.finalView
        || !current
        || current.revision !== revision
        || (await p2pFinalStateCommitment(current.snapshot.state)
          !== await p2pFinalStateCommitment(delivery.finalView))
      ) {
        throw new AdapterError("P2P_ERROR", `Full terminal view does not match seat ${playerId}`, false);
      }
      states.set(playerId, delivery.finalView);
    }
    return states;
  }

  terminalDisplay(): FullTerminalDelivery["display"] | null {
    return this.terminalDeliveries.get(0)?.display ?? null;
  }

  async acknowledgeTerminalDeliveries(revision: number): Promise<void> {
    const deliveries = await this.readTerminalDeliveries();
    if (!deliveries) throw new Error("Full terminal delivery is unavailable for acknowledgement");
    for (const delivery of deliveries.values()) {
      if (delivery.terminalRevision !== revision) throw new Error("Full terminal revision changed before acknowledgement");
      if (!(await acknowledgeFullTerminalDelivery(
        "native-engine://phase-server",
        delivery.deliveryId,
        delivery.credential,
        () => new NativeEngineSocket(),
      ))) {
        throw new Error("Full terminal delivery acknowledgement was rejected");
      }
    }
  }

  private async restoreTerminalViews(): Promise<boolean> {
    const deliveries = await this.readTerminalDeliveries();
    if (!deliveries) return false;
    const first = deliveries.values().next().value as FullTerminalDelivery | undefined;
    if (!first?.finalView) return false;
    for (const client of this.clients.values()) client.dispose();
    this.clients.clear();
    const revision = first.terminalRevision;
    for (const [playerId, delivery] of deliveries) {
      if (
        delivery.terminalRevision !== revision
        || !delivery.finalView
        || delivery.finalView.waiting_for.type !== "GameOver"
      ) {
        throw new AdapterError("P2P_ERROR", `Full terminal view is invalid for seat ${playerId}`, false);
      }
      this.seedTerminalView(playerId, delivery.finalView, revision);
    }
    return true;
  }

  /** Record a seat's recipient-bound Full final view as its latest view at the terminal revision. */
  private seedTerminalView(playerId: PlayerId, finalView: GameState, revision: number): void {
    this.latestViews.set(playerId, {
      snapshot: {
        state: finalView,
        legalResult: EMPTY_LEGAL_ACTIONS,
        seq: nextSnapshotSeq(),
      },
      events: [],
      revision,
    });
  }

  private async readTerminalDeliveries(): Promise<Map<PlayerId, FullTerminalDelivery> | null> {
    if (!this.fullKey || this.playerTokens.size === 0) return null;
    const received = await Promise.all([...this.playerTokens].map(async ([playerId, token]) => {
      const cached = this.terminalDeliveries.get(playerId);
      if (cached) return [playerId, cached] as const;
      const delivery = await bootstrapFullTerminalDelivery(
        "native-engine://phase-server",
        this.fullKey!,
        token,
        crypto.randomUUID(),
        () => new NativeEngineSocket(),
      );
      return delivery ? [playerId, delivery] as const : null;
    }));
    const found = received.filter((entry): entry is readonly [PlayerId, FullTerminalDelivery] => entry !== null);
    if (found.length === 0) return null;
    if (found.length !== this.playerTokens.size) {
      throw new AdapterError("P2P_ERROR", "Full terminal authority is incomplete across native seats", true);
    }
    const deliveries = new Map<PlayerId, FullTerminalDelivery>(found);
    const first = deliveries.values().next().value as FullTerminalDelivery | undefined;
    if (!first) return null;
    for (const [playerId, delivery] of deliveries) {
      if (
        delivery.key.game_code !== this.fullKey.game_code
        || delivery.key.generation !== this.fullKey.generation
        || delivery.terminalRevision !== first.terminalRevision
        || !delivery.finalView
        || delivery.finalView.waiting_for.type !== "GameOver"
        || delivery.display.winner !== first.display.winner
        || delivery.display.reason !== first.display.reason
      ) {
        throw new AdapterError("P2P_ERROR", `Full terminal authority is invalid for seat ${playerId}`, false);
      }
    }
    for (const [playerId, delivery] of deliveries) this.terminalDeliveries.set(playerId, delivery);
    return deliveries;
  }

  private async reconnectClient(
    playerId: PlayerId,
    playerToken: string,
  ): Promise<NativeSessionAttachment> {
    if (!this.gameCode || !this.fullKey) {
      throw new AdapterError("P2P_ERROR", "Native game code is unavailable for reconnect", false);
    }
    const client = new WebSocketAdapter(
      "native-engine://phase-server",
      "join",
      { main_deck: [], sideboard: [] },
      undefined,
      undefined,
      undefined,
      `Player ${playerId + 1}`,
      {
        nativePregame: {
          kind: "reconnect",
          socketFactory: () => new NativeEngineSocket(),
          expectedServerVersion: this.options.expectedServerVersion,
          callerManagedTerminalDelivery: true,
          gameCode: this.gameCode,
          playerId,
          playerToken,
          fullKey: this.fullKey,
        },
      },
    );
    const previous = this.clients.get(playerId);
    const attachment = await this.attachClient(client);
    if (attachment.playerId !== playerId) {
      client.dispose();
      throw new AdapterError("P2P_ERROR", "Native reconnect returned the wrong player seat", false);
    }
    if (previous && previous !== client) previous.dispose();
    return attachment;
  }

  /** Freshly replaces this Full seat's sender before reading Concede reality. */
  async reconcileConcede(playerId: PlayerId): Promise<NativeViewerUpdate> {
    const token = this.playerTokens.get(playerId);
    if (!token) throw new AdapterError("P2P_ERROR", `No native token for seat ${playerId}`, false);
    try {
      await this.reconnectClient(playerId, token);
      await this.revisionQueue;
    } catch (reconnectError) {
      if (!(await this.restoreTerminalViews())) throw reconnectError;
    }
    let update = this.latestViews.get(playerId);
    if (update?.snapshot.state.waiting_for.type === "GameOver") {
      if (!(await this.restoreTerminalViews())) {
        throw new AdapterError("P2P_ERROR", "Terminal Concede has no retained Full final view", true);
      }
      update = this.latestViews.get(playerId);
    }
    if (!update) throw new AdapterError("P2P_ERROR", `No authoritative state for seat ${playerId}`, true);
    return update;
  }

  /** One contest click for `playerId`, sent on that seat's own authenticated socket. */
  submitContestClick(playerId: PlayerId, contest: number, seq: number): void {
    this.clients.get(playerId)?.submitContestClick(contest, seq);
  }

  /** No target: the server rolls back the last action. */
  requestTakeback(playerId: PlayerId): void {
    this.clients.get(playerId)?.sendRequestTakeback();
  }

  respondTakeback(playerId: PlayerId, approve: boolean): void {
    this.clients.get(playerId)?.sendRespondTakeback(approve);
  }

  cancelTakeback(playerId: PlayerId): void {
    this.clients.get(playerId)?.sendCancelTakeback();
  }

  private clientFor(playerId: PlayerId): WebSocketAdapter {
    const client = this.clients.get(playerId);
    if (!client) {
      throw new AdapterError("P2P_ERROR", `No native socket for seat ${playerId}`, false);
    }
    return client;
  }
}

function isDeckListPlayerShape(x: unknown): x is DeckListPayload["player"] {
  return (
    x !== null &&
    typeof x === "object" &&
    "main_deck" in x &&
    Array.isArray((x as { main_deck: unknown }).main_deck)
  );
}

/**
 * Game-run state. Typed enum (per CLAUDE.md §4: no raw bool flags).
 * - `running`     — normal play, `submitAction` accepted.
 * - `paused-disconnect` — automatic pause due to a guest dropping; auto-resumes
 *   on reconnect or auto-concedes at grace expiry. Blocks `submitAction`.
 * - `paused-manual` — host-initiated pause (either via "Pause and wait" on the
 *   disconnect dialog, or an explicit pause request). Released by host or by
 *   the dropped player reconnecting (see plan §6 DisconnectChoiceDialog
 *   semantics). Blocks `submitAction`.
 */
type GameRunState = "running" | "paused-disconnect" | "paused-manual" | "terminal";

interface BrowserTransition {
  result: SubmitResult;
  generation: number;
}

interface ProjectedBrowserTransition {
  snapshot: ViewerSnapshot;
  events: GameEvent[];
  fresh: boolean;
}

/** Default grace window for guest auto-reconnect, in milliseconds. */
const DEFAULT_GRACE_PERIOD_MS = 30_000;

/**
 * Guest auto-reconnect backoff schedule. Escalates briskly for early
 * attempts (WiFi blip case), then levels at 60s for the long tail.
 * After the explicit schedule, retries continue at `RECONNECT_STEADY_STATE_MS`
 * indefinitely until the adapter is `terminated` (explicit user leave).
 *
 * This tolerates host-resume scenarios where the host is down for
 * several minutes (browser crash + reopen + reconnect all happen
 * asynchronously). Giving up after 80s — the prior schedule — would
 * orphan guests whose host is in the middle of a legitimate resume.
 */
const RECONNECT_BACKOFF_MS = [1_000, 2_000, 4_000, 8_000, 15_000, 30_000, 60_000];
const RECONNECT_STEADY_STATE_MS = 60_000;

/**
 * Budget for a guest's in-flight submission — the window between sending an
 * `action`/`interaction` frame and the host reply that settles it.
 *
 * Sizing input is host turnaround: a WASM engine submit plus a per-guest
 * viewer snapshot, on a possibly-mobile host with a large Commander board.
 * 30s is chosen so a merely slow host is never failed. The timer is a bound on
 * an unanswerable wait; it is deliberately too coarse to be a latency signal.
 *
 * One legitimate wait can still exceed it: the host answers a guest action
 * before `runAiLoop()`, but `runAiLoop` drives the SAME engine-worker client,
 * and a long WASM AI search blocks that worker's event loop. A guest action
 * arriving during one queues behind it, and a multi-minute Commander decision
 * is on record. The degradation there is graceful rather than lossy — the guest
 * gets a recoverable toast, and the late `state_update` still lands and
 * resyncs through `processRemoteUpdate`.
 *
 * ## Why a timer at all
 *
 * The #8360 acceptance ledger covers ONE of the four frame types that settle a
 * guest submission. `state_update` is acked (`sendStateAck`), tracked
 * (`recordGuestAck`) and resent by the host's redelivery sweep.
 * `action_rejected` and `action_failed` have no ack, no ledger entry and no
 * redelivery — `redeliverGuestState` sends only `state_update` and
 * `terminal_result` — and the host's `send` returns `Promise<boolean>` without
 * throwing, a boolean the `action_*` dispatch sites discard. (`action_noop` is
 * uncovered too, but it is debug-only — `isZeroCountDebugCreate` — so there are
 * two reachable uncovered classes, not three.)
 *
 * The primary measured route is the host lease fence. `ownsAuthority()` is
 * `ownsP2PHostLease()` plus a trace, and `ownsP2PHostLease`
 * (`services/p2pSession.ts`) re-reads the lease on EVERY call, so it can flip
 * during an await; on `false` it only traces and never closes the channel. So
 * the guest's action APPLIES, and then `broadcastStateUpdateInner` bails on
 * `!ownsAuthority()` before `++authoritativeRevision`, with
 * `queueLaggingRedeliveries`, `redeliverGuestState` and `send()` fenced by the
 * same predicate. The action has applied, but no frame goes out and the
 * revision never rises, so the sweep is disarmed and the channel stays healthy
 * and ponging. A liveness detector therefore has nothing to find, and the guest
 * waits forever while holding the module-level dispatch mutex.
 *
 * Reachability precondition: tab A's PeerJS SIGNALING socket must have dropped
 * (freeing the peer id) while its WebRTC DataConnections stay up. That state is
 * possible while signaling recovery retries after sleep, a network change,
 * or mobile backgrounding. Opening a second tab does NOT reach it on its own: `hostRoom`
 * opens the peer before the adapter is constructed, so tab B fails
 * `unavailable-id` and never reaches `claimP2PHostLease`. The lease flip must
 * land inside the `submitAction` → `broadcastStateUpdateInner` window, so
 * exactly ONE in-flight submission is stranded — matching a game that freezes
 * once, mid-match.
 *
 * ## Why reject, when this repo's convention for gameplay round-trips is notify
 *
 * `engine-worker-client.ts` deliberately went the other way:
 * `ENGINE_REQUEST_TIMEOUT_MS = 60_000` with `RequestTimeoutBehavior` defaulting
 * to `"notify"` for gameplay, `"reject"` reserved for init, after
 * reject-on-timer for gameplay was walked back. P2P differs in the one way that
 * matters: after this rejection `pendingResolve` is null, so a late
 * `state_update` still lands — the handler emits `stateChanged`, which
 * `GameProvider` routes into `processRemoteUpdate`. The late reply is NOT lost.
 * At the worker boundary it would have been, which is exactly why "notify" won
 * there.
 *
 * The code is `P2P_ERROR`, not `ENGINE_UNRESPONSIVE`: the latter is suppressed
 * by `shouldShowActionError` in `dispatch.ts` and routes to `notifyEngineLost`,
 * the wrong surface for a peer that may simply be gone.
 *
 * ## Honest residual
 *
 * Rejecting UNFREEZES the client but does not RESYNC it. On the lease-fence
 * route the guest's cached state is STALE — the action applied on a host that
 * then sent nothing — so a post-timeout retry re-submits against a board the
 * guest has wrong; the user's recovery is a reload, not a retry. The
 * stale-state watchdog cannot help: for a guest, the adapter cache and the
 * screen go stale together, so the fingerprints match and its check returns.
 *
 * A SECOND residual is new here, and it belongs to the single unkeyed pending
 * slot. Once this timer has rejected submission A, a retry B parks in that same
 * slot, and A's late `state_update` settles B: its revision is NEWER than the
 * guest's cached one, so the stale-revision guard above does not drop it. B's
 * own frame then arrives, finds nothing pending, and emits `stateChanged`, so
 * the board still converges — the cost is one early resolve carrying another
 * action's events. Routing replies correctly needs a wire request id echoed on
 * every settlement frame, i.e. a `WIRE_PROTOCOL_VERSION` bump, and is deferred.
 * Do NOT instead widen the revision guard to drop such frames: dropping the
 * frame that reports application is the deadlock the acceptance ledger exists
 * to end.
 */
const SUBMISSION_TIMEOUT_MS = 30_000;
// A stale proposal leaves the prompt unchanged, so cap retries to prevent a
// persistent authority race from becoming a tight host-loop spin.
const MAX_AI_PROPOSAL_STALE_RETRIES = 3;

/**
 * Preserve the engine's structured, viewer-filtered rejection exactly. All
 * other failures are transport/operational faults and must not masquerade as
 * gameplay diagnostics on the guest.
 */
function actionFailureFrame(error: unknown): Extract<P2PMessage, { type: "action_rejected" | "action_failed" }> {
  if (error instanceof AdapterError && isActionRejection(error.rejection)) {
    return { type: "action_rejected", rejection: error.rejection };
  }
  return {
    type: "action_failed",
    message: error instanceof Error ? error.message : String(error),
  };
}

function manaPaymentPreviewFailureFrame(
  requestId: number,
  error: unknown,
): Extract<P2PMessage, { type: "mana_payment_preview_rejected" | "mana_payment_preview_failed" }> {
  if (error instanceof AdapterError && isActionRejection(error.rejection)) {
    return { type: "mana_payment_preview_rejected", requestId, rejection: error.rejection };
  }
  return {
    type: "mana_payment_preview_failed",
    requestId,
    message: error instanceof Error ? error.message : String(error),
  };
}

function defaultSeatState(playerCount: number, formatConfig?: FormatConfig): SeatState {
  return {
    seats: [
      { type: "HostHuman" },
      ...Array.from({ length: playerCount - 1 }, () => ({ type: "WaitingHuman" as const })),
    ],
    tokens: Array.from({ length: playerCount }, (_, idx) => (idx === 0 ? "host" : "")),
    format: formatConfig ?? {
      format: "Standard",
      starting_life: 20,
      min_players: 2,
      max_players: 2,
      deck_size: { type: "Minimum", data: 60 },
      singleton: false,
      command_zone: false,
      commander_damage_threshold: null,
      range_of_influence: null,
      team_based: false,
      sideboard_policy: { type: "Limited", data: 15 },
      default_deck_copy_limit: { type: "UpTo", data: 4 },
      uses_commander: false,
      allow_debug_actions: false,
    },
    gameStarted: false,
  };
}

function seatStateToView(state: SeatState): SeatView {
  return {
    seats: state.seats,
    format: state.format,
    isFull: state.seats.every((seat) => seat.type !== "WaitingHuman"),
    gameStarted: state.gameStarted,
  };
}

function occupiedSeatCount(state: SeatState): number {
  return state.seats.filter((seat) => seat.type !== "WaitingHuman").length;
}

export function aiActorFromWaitingFor(
  waitingFor: WaitingFor,
  seats: SeatState["seats"],
  authorizedSubmitter: PlayerId,
): PlayerId | null {
  if (
    waitingFor.type === "MulliganDecision" ||
    waitingFor.type === "OpeningHandBottomCards"
  ) {
    return (
      waitingFor.data.pending.find((entry) => seats[entry.player]?.type === "Ai")
        ?.player ?? null
    );
  }

  // CR 723.5: Under a turn-control effect (Emrakul, the Promised End / Worst
  // Fears / Mindslaver) the seat that must *submit* this decision is the
  // authorized submitter, NOT the semantic acting player
  // (`waiting_for.data.player`, which is the controlled seat). The engine is the
  // single authority and re-derives `priority_player` to the authorized
  // submitter (`crates/engine/src/game/public_state.rs`). Routing the host AI
  // loop off `data.player` would `submitAction` as the controlled seat, which
  // the engine rejects with `WrongPlayer`, stalling the controlled turn in
  // multiplayer. This mirrors the `aiController.ts` fix for #2012. With no
  // turn-control effect, `priority_player === data.player` for every
  // single-acting state, so this is a no-op.
  // CR 732.2a: LoopShortcut's data field is `proposer`, not `player`; route to
  // the engine-derived authorized submitter (priority_player) exactly like the
  // `player in` states so an AI-owned controller seat drives the declare.
  return "player" in waitingFor.data
    || waitingFor.type === "LoopShortcut"
    || waitingFor.type === "PrecastCopyShortcutOffer"
    ? authorizedSubmitter
    : null;
}

export function playerSlotsFromSeatView(view: SeatView): PlayerSlot[] {
  return view.seats.map((kind, playerId) => ({
    playerId,
    kind,
    teamInfo: view.teamInfo?.[playerId] ?? undefined,
    name:
      playerId === 0
        ? "Host"
        : kind.type === "Ai"
          ? `AI (${kind.data.difficulty})`
          : kind.type === "WaitingHuman"
            ? ""
            : `Player ${playerId + 1}`,
  }));
}

function traceAdapter(side: "Host" | "Guest", event: string, data?: Record<string, unknown>): void {
  console.debug(`[P2P ${side} Adapter]`, performance.now().toFixed(1), event, data ?? {});
}

function resolvedTakebackEvent(notice: TakebackNotice): TakebackEvent {
  return { type: "takebackResolved", resolution: notice.resolution, resolvedBy: notice.resolvedBy };
}

function isZeroCountDebugCreate(action: GameAction): boolean {
  if (action.type !== "Debug") return false;
  switch (action.data.type) {
    case "CreateCard":
    case "CreateToken":
    case "CreateTokenCopy":
      return action.data.data.count === 0;
    default:
      return false;
  }
}

/**
 * The host session, if any, that currently owns the engine's game state.
 *
 * On a memory-constrained device `getHostAdapter()` hands every host the tab's
 * shared engine worker, so "who installed the state that is there now?" stops
 * being answerable from the adapter's own fields. The claim is recorded only
 * once an engine call has *accepted* it (after `initializeGame` on the fresh
 * start arm, after `resumeMultiplayerHostState` on the resume arm) — claiming
 * earlier would let a refused resume's teardown wipe a live local game it never
 * owned. Teardown clears engine state only for the current claimant, so a stale
 * or never-started host cannot clobber the live one.
 *
 * Holds each host's claim token rather than the adapter itself, so a claim can
 * never keep a torn-down host — with its guest sessions and deck payloads —
 * resident in a module-level reference.
 */
let sharedEngineHost: symbol | null = null;

/**
 * Cadence of the host's per-guest state redelivery sweep — deliberately the
 * same 5s as transport latency sampling (`network/peer.ts`). WebRTC close/error
 * detects channel loss; stale pongs only clear the latency display. This sweep
 * heals unacknowledged state on a live channel, one queued/in-flight resend
 * per seat, without replaying the missed frame's events or logs.
 */
const STATE_REDELIVERY_TICK_MS = 5_000;

/**
 * Cadence of the host's own publication retry. A host snapshot read that failed after
 * the engine accepted a transition leaves that transition owed to the host's store;
 * this bounds how long the host screen can stay behind its engine with nobody else to
 * deliver it. Short, because the host screen is the one a player is staring at.
 */
const HOST_PUBLICATION_RETRY_TICK_MS = 1_000;

/**
 * Fail-loud contract for a disposed host. With a private worker, `dispose()`
 * tore the engine down and every later call threw `assertInitialized`. A shared
 * worker survives disposal, so a use-after-dispose host would silently operate
 * on the live shared engine instead.
 */
function hostDisposedError(): AdapterError {
  return new AdapterError("P2P_ERROR", "P2P host adapter has been disposed", false);
}

/**
 * Host-side P2P adapter.
 *
 * Hub-and-spoke topology: the host runs the authoritative engine (WASM by
 * default, or a local native phase-server when configured) and maintains one
 * `PeerSession` per guest. State updates are filtered per-seat and fanned out
 * to each guest. Guest actions are authenticated by their host-owned session
 * before reaching the selected authority.
 *
 * The host does NOT destroy the parent `Peer` on per-session disconnects —
 * that lifetime is owned by `dispose()`. Per-session cleanup releases only
 * the `DataConnection` (see `peer.ts` `onSessionEnd` contract).
 */
export class P2PHostAdapter implements EngineAdapter {
  private wasm = getHostAdapter();
  /** Caller-held lease for the current WASM host attempt. The shared adapter
   * can serve several hosts over its lifetime, so teardown must use this
   * exact handle rather than whatever owner started later. */
  private wasmHostOwner: HostSessionOwner | null = null;
  private nativeBridge: NativeP2PBridge | null = null;
  /** Present for a P2P host, whether its authority is browser WASM or native. */
  exportPersistenceState?: () => Promise<string>;
  private nativeInitialSetupPending = false;
  private listeners: P2PAdapterEventListener[] = [];
  /**
   * Mirrors WasmAdapter's initialization contract: setup runs exactly once,
   * and concurrent callers share its in-flight promise. The lobby initializes
   * the host before advertising it; the game-page handoff later calls
   * initialize again while seeding gameStore.
   */
  private initialized = false;
  private initPromise: Promise<void> | null = null;
  /**
   * Set synchronously by `dispose()`. Read by every engine entry point (so a
   * disposed host fails loud even when its engine is the shared worker that
   * outlives it) and re-checked after each await in the init/start paths, so a
   * teardown that lands mid-flight cannot be overtaken by a resumed claim.
   * Deliberately not `ownsAuthority()`: `dispose()` releases the host lease, so
   * the lease says nothing about *this* adapter having been torn down.
   */
  private disposed = false;
  /** This session's identity in `sharedEngineHost`. */
  private readonly engineClaim = Symbol("p2p-host-engine-claim");

  private guestSessions = new Map<PlayerId, PeerSession>();
  /** Guests' contest clicks awaiting the next `driveContest` tick, gated to the
   *  running contest's seats and bounded per seat. */
  private readonly guestContestClicks = new ContestClickBuffer();
  /**
   * Accepted transitions whose state/events never reached the host's own store
   * because the snapshot read failed. The engine has already moved on, so nothing
   * can recreate them: they are held here and delivered, in order and exactly once,
   * with the next successful host publication.
   */
  private unpublishedHostTransitions: SubmitResult[] = [];
  /** A settled contest's AI follow-up is owed once its resolution has been published. */
  private aiFollowUpOwedAfterPublication = false;
  /**
   * The terminal statement of a game whose retained transitions could not be published
   * when it closed. The host's `terminalResult` emit unmounts the board, so it waits
   * until the host's store has seen every accepted transition.
   */
  private pendingHostTerminalEmit: P2PTerminalResult | null = null;
  /** The in-flight terminal commit; a second caller shares its outcome. */
  private terminalCommitInFlight: Promise<boolean> | null = null;
  /** Serializes host publications, so a retry never interleaves with another publisher. */
  private hostPublicationQueue: Promise<void> = Promise.resolve();
  /** The in-flight settlement of owed host publications; concurrent callers share it. */
  private hostSettlement: Promise<boolean> | null = null;
  /** Owned by the adapter, not by any overlay: armed whenever a publication fails. */
  private hostPublicationTimer: ReturnType<typeof setInterval> | null = null;
  private sessionLatencies = new WeakMap<PeerSession, number | null>();
  /**
   * A reconnecting transport has proved its token but is not a game session
   * until its complete reconnect acknowledgement has been written. This is
   * deliberately adapter-ephemeral: persisting an in-flight channel would
   * make a reload retain a connection that cannot exist any more.
   */
  private pendingReconnectSessions = new Map<PlayerId, PeerSession>();
  private pendingTerminalReconnectAcks = new Map<PlayerId, {
    session: PeerSession;
    revision: number;
    snapshot: EngineSnapshot;
    ready: boolean;
    stateAckReceived: boolean;
    finishing: boolean;
  }>();
  /** Native snapshots become reconnectable only after their matching server
   * revision has completed the PeerJS fan-out. */
  private nativeDeliveredViews = new Map<PlayerId, { revision: number; snapshot: EngineSnapshot }>();
  /** The notice a seat was handed instead of its latest native revision; cleared by its next delivered view. */
  private nativeWithheldSeats = new Map<PlayerId, StateWithheldNotice>();
  /** Serializes every state-bearing delivery with reconnect promotion. A
   * reconnect cannot acknowledge an older view while a state fan-out is in
   * flight and would otherwise miss that update after becoming active. */
  private deliveryQueue: Promise<void> = Promise.resolve();
  private guestDecks = new Map<PlayerId, DeckListPayload["player"]>();
  private aiDecks = new Map<PlayerId, DeckListPayload["player"]>();
  private playerTokens = new Map<PlayerId, string>();
  /**
   * Mid-game disconnect tracker. `timer` is nullable: it is set when the grace
   * window is armed (auto-concede on expiry) and nulled by `holdForReconnect`
   * (indefinite wait). Using `Timer | null` in the shape instead of a cast
   * keeps the "manual pause" transition type-honest (per CLAUDE.md: no raw
   * bool flags, no cast-arounds).
   */
  private disconnectedSeats = new Map<
    PlayerId,
    { disconnectedAt: number; timer: ReturnType<typeof setTimeout> | null }
  >();
  private kickedTokens = new Set<string>();
  /**
   * Seats whose engine `PlayerId` has been conceded (CR 800.4a). Populated by
   * `concedePlayer`; used by `handleGuestMessage` to short-circuit actions
   * from already-eliminated guests without a WASM round-trip.
   */
  private eliminatedSeats = new Set<PlayerId>();
  // Temporary admission fence, never persisted as gameplay elimination.
  private concedingSeats = new Set<PlayerId>();
  // A later explicit departure request must recheck Full authority before it
  // can submit another Concede after an unresolved transport outcome.
  private uncertainConcedeSeats = new Set<PlayerId>();
  private gameRunState: GameRunState = "running";
  /** Monotonic authority revision for WASM hosts; native hosts replace this
   * with the local phase-server's revision before fan-out. */
  private authoritativeRevision = 0;
  /** Browser-only fence for pairing an accepted transition with its projection. */
  private browserMutationGeneration = 0;
  /**
   * How far along each guest seat is, on two different signals — because the
   * two failure directions are not symmetric:
   *
   *  - ADVANCE is acceptance-only (`recordGuestAck`, driven by the seat's own
   *    `state_ack`). A `send` resolving true only means the bytes reached the
   *    channel, and peerjs parks anything past its buffered-amount budget in a
   *    buffer that `close()` discards. A ledger that advanced on transmission
   *    therefore marked a seat delivered while the host was still waiting on
   *    that seat to act — the sweep skipped it and the match deadlocked.
   *  - CREATE is transmission-only (`seedGuestEntry`, on an accepted
   *    `game_setup` or `reconnect_ack` send). The asymmetry rests entirely on
   *    the false-NEGATIVE side: a seat with no entry is one the sweep will
   *    never nominate, so losing the create strands a seat that could have
   *    been healed. A false POSITIVE is NOT self-healing — a parked handshake
   *    never assigns the guest's `authenticatedSession`, so every redelivered
   *    `state_update` is discarded unauthenticated — it is merely no worse
   *    than the handshake never having been sent, which is already
   *    unrecoverable.
   *
   * A missing entry therefore means the seat was never HANDED a handshake
   * frame its channel accepted — no `game_setup`, no `reconnect_ack`. The
   * redelivery sweep skips such seats, because a `state_update` cannot
   * substitute for the setup handshake.
   */
  private guestAckedRevisions = new Map<PlayerId, number>();
  /** Exact recipient statements issued so a receipt can be checked even if it
   * arrives while the host's send promise is still settling. */
  private issuedTerminalResults = new Map<PlayerId, P2PTerminalResult>();
  /** A durable guest receipt, not channel acceptance, releases native payloads. */
  private terminalResultReceipts = new Set<PlayerId>();
  /** Retain authenticated recipient identity across a close racing receipt decode. */
  private terminalReceiptSessions = new WeakMap<PeerSession, PlayerId>();
  private nativeTerminalAckReady = false;
  private nativeTerminalPersistenceClearing = false;
  private nativeTerminalPersistenceCleared = false;
  private nativeTerminalAcknowledged = false;
  private nativeTerminalAckPromise: Promise<void> | null = null;
  private redeliveryTimer: ReturnType<typeof setInterval> | null = null;
  private pendingRedeliveries = new Set<PlayerId>();
  /** First committed terminal statement fences every subsequent action and
   * reconnect. Its id is immutable for this adapter incarnation. */
  private terminalResult: P2PTerminalResult | null = null;
  /** Durable local terminal statement, supplied only as transient native-resume input. */
  private resumeTerminalResult: P2PTerminalResult | null = null;
  /** Native AI faults are terminal and must also be replayed to a guest that
   * reconnects after the live PeerJS fan-out completed. */
  private nativeAiDriverFault: NativeAiDriverFault | null = null;
  /** The local host must render a restored native fault once the bridge has
   * delivered its fenced final snapshot. Keep this separate from the durable
   * fault itself: rehydration sets the latter before the bridge replays it. */
  private deliveredNativeAiDriverFault: NativeAiDriverFault | null = null;
  readonly supportsMatchConcede: true | undefined;
  readonly supportsTakeback = true;
  private matchConcedeSent = false;

  private gameStarted = false;
  private guestDeckResolvers: Array<() => void> = [];
  private hostConnectionUnsub: (() => void) | null = null;
  private guestNames = new Map<PlayerId, string>();
  private closedPregameSessions = new WeakSet<PeerSession>();
  private hostDisplayName: string | null = null;
  private pregameSeatState: SeatState;
  private pregameSeatView: SeatView;
  private pregameOpQueue: Promise<void> = Promise.resolve();
  private resolvePregameReady!: () => void;
  private rejectPregameReady!: (err: unknown) => void;
  private pregameReady!: Promise<void>;
  private allowPartialStart = false;

  /**
   * Identifier used as the key when this adapter writes its resume
   * metadata via `saveP2PHostSession`. Absent means the adapter is
   * running without persistence (tests, ephemeral hosts) — save-hooks
   * short-circuit as no-ops.
   */
  private readonly gameId: string | null;
  /** Bare 5-char room code without PEER_ID_PREFIX — persisted in the session record. */
  private readonly roomCode: string | null;
  /** Stable identity is retained on resume; the incarnation fences old hosts. */
  private readonly sessionKey: P2PSessionKey;
  private readonly authority: P2PAuthorityStamp;
  /** True when the adapter was constructed from a persisted session (resume flow). */
  private readonly isResume: boolean;
  /**
   * Pending GameState snapshot to hand to `wasm.resumeMultiplayerHostState`
   * during `initialize()`. Set in the constructor from `resumeData.state`;
   * nulled after the WASM call consumes it. Held on the adapter rather
   * than threaded through `initialize()` so the EngineAdapter interface
   * stays uniform across fresh/resume flows.
   */
  private resumeGameState: PersistedGameState | null = null;
  /** The exactly-once result produced while installing a persisted host. */
  private resumedAutomation: RestoredGameStateResult | null = null;

  constructor(
    private readonly hostDeckData: unknown,
    private readonly hostPeer: TransportPeer,
    /**
     * Subscribe to inbound guest `DataConnection`s via `hostRoom()`'s
     * documented API. Using this (instead of `hostPeer.on("connection")`
     * directly) avoids double-dispatch with `hostRoom()`'s internal
     * listener, and drains any connections that were buffered while the
     * adapter was still under construction.
     */
    private readonly onGuestConnected: (
      handler: (conn: TransportConnection) => void,
    ) => () => void,
    private readonly playerCount: number,
    private readonly formatConfig?: FormatConfig,
    private readonly matchConfig?: MatchConfig,
    private readonly gracePeriodMs: number = DEFAULT_GRACE_PERIOD_MS,
    /**
     * Optional broker that registered this room's lobby entry. When set,
     * the adapter fires `broker.unregister(brokerGameCode)` after a
     * successful `initializeGame` so the public listing disappears as
     * soon as the engine is live. Absent for legacy pure-PeerJS rooms
     * where no server-side listing exists.
     */
    private readonly broker?: BrokerClient,
    private readonly ownsBroker: boolean = true,
    /**
     * Server-assigned game code for the lobby entry the broker holds.
     * Required when `broker` is set; unused otherwise. Distinct from the
     * PeerJS peer ID the guest dials over.
     */
    private readonly brokerGameCode?: string,
    /**
     * Persistence binding for host resume. When provided, the adapter
     * writes a `PersistedP2PHostSession` snapshot at every lifecycle
     * event (guest join, reconnect, game start, kick, concede) so a
     * crashed/reloaded host can come back on the same room code.
     *
     * `resumeData` carries a prior session to rehydrate (for resume
     * flows) — the engine state is separately loaded via
     * `wasm.resumeMultiplayerHostState` in `initialize()`.
     */
    persistence?: {
      gameId: string;
      roomCode: string;
      hostDisplayName?: string;
      resumeData?: {
        state?: PersistedGameState;
        session: PersistedP2PHostSession;
        terminalResult?: P2PTerminalResult;
      };
    },
    native?: NativeP2PHostOptions,
    private readonly boundMatchConcede?: BoundP2PMatchConcede,
  ) {
    this.supportsMatchConcede = boundMatchConcede ? true : undefined;
    if (playerCount < 2 || playerCount > 6) {
      throw new AdapterError(
        "P2P_PLAYER_COUNT",
        `P2P supports 2-6 players; got ${playerCount}`,
        false,
      );
    }
    if (broker && !brokerGameCode) {
      throw new AdapterError(
        "P2P_BROKER_CONFIG",
        "brokerGameCode is required when broker is provided",
        false,
      );
    }
    this.pregameSeatState = defaultSeatState(playerCount, formatConfig);
    this.pregameSeatView = seatStateToView(this.pregameSeatState);
    this.gameId = persistence?.gameId ?? null;
    this.roomCode = persistence?.roomCode ?? null;
    this.sessionKey = persistence?.resumeData?.session.sessionKey ?? createP2PSessionKey();
    this.authority = claimP2PHostLease(this.sessionKey);
    this.hostDisplayName = persistence?.hostDisplayName ?? null;
    this.isResume = persistence?.resumeData !== undefined;

    if (persistence?.resumeData) {
      this.resumeGameState = persistence.resumeData.state ?? null;
      this.resumeTerminalResult = persistence.resumeData.terminalResult ?? null;
      this.rehydrateFromPersistedSession(persistence.resumeData.session);
      this.pregameSeatView = seatStateToView(this.pregameSeatState);
    }
    const nativeResume = persistence?.resumeData?.session.nativeSession;
    if (this.resumeTerminalResult && !nativeResume) {
      throw new AdapterError(
        "P2P_ERROR",
        "A retained terminal result requires its native Full resume session",
        false,
      );
    }
    if (native && persistence?.resumeData && !nativeResume) {
      throw new AdapterError(
        "P2P_ERROR",
        "Native P2P sessions cannot resume through a persisted WASM snapshot",
        false,
      );
    }
    if (!native && nativeResume) {
      throw new AdapterError(
        "P2P_ERROR",
        "This hosted game must reconnect to its local native engine",
        false,
      );
    }
    if (native) {
      this.nativeBridge = new NativeP2PBridge(
        hostDeckData as DeckListPayload,
        this.hostDisplayName ?? "Host",
        playerCount,
        formatConfig,
        matchConfig,
        native,
        (revision, reports) => this.enqueueDelivery(
          () => this.handleNativeRevision(revision, reports),
        ),
        (fault) => this.enqueueDelivery(() => this.handleNativeAiDriverFault(fault)),
        (revision) => this.enqueueDelivery(() => this.commitWithheldHostTerminal(revision)),
        (pid, event) => this.enqueueDelivery(() => this.handleNativeSeatNotice(pid, event)),
        // A returning guest's replay arrives over its new loopback before its session is promoted.
        (pid) => this.guestSessions.get(pid) ?? this.pendingReconnectSessions.get(pid),
        nativeResume,
      );
    }
    this.attachAuthorityDiagnostics();
  }

  /**
   * Installs the local authority diagnostics capability. Native hosts request
   * the server's trusted envelope; browser hosts obtain it from their WASM
   * engine. P2P guests never construct this adapter.
   */
  private attachAuthorityDiagnostics(): void {
    this.exportPersistenceState = async () => {
      this.assertNotDisposed();
      if (!this.ownsAuthority()) {
        throw new AdapterError("P2P_ERROR", "P2P host authority changed", false);
      }
      if (this.nativeBridge) return this.nativeBridge.exportPersistenceState();
      return this.wasm.exportPersistenceState();
    };
    if (this.nativeBridge) return;
    Object.assign(this, {
      setAiDecisionDiagnosticsEnabled: (enabled: boolean) =>
        this.wasm.setAiDecisionDiagnosticsEnabled(enabled),
      subscribeAiDecisionDiagnostics: (listener: (receipt: AiDecisionDiagnosticReceipt) => void) =>
        this.wasm.subscribeAiDecisionDiagnostics(listener),
    });
  }

  /**
   * Restore in-memory adapter maps from a persisted session so the
   * resumed host agrees with its guests about seat assignments,
   * kicked tokens, and eliminated players. Called from the constructor
   * when `resumeData` is provided.
   *
   * Engine state is restored separately via
   * `wasm.resumeMultiplayerHostState` in `initialize()` — this method
   * only handles adapter-owned transport + security state.
   */
  private rehydrateFromPersistedSession(session: PersistedP2PHostSession): void {
    if (session.seatState) {
      this.pregameSeatState = session.seatState;
    }
    for (const [pidStr, token] of Object.entries(session.playerTokens)) {
      this.playerTokens.set(Number(pidStr), token);
    }
    for (const [pidStr, deck] of Object.entries(session.guestDecks)) {
      if (isDeckListPlayerShape(deck)) {
        this.guestDecks.set(Number(pidStr), deck);
      }
    }
    for (const [pidStr, deck] of Object.entries(session.aiDecks ?? {})) {
      if (isDeckListPlayerShape(deck)) {
        this.aiDecks.set(Number(pidStr), deck);
      }
    }
    // A returning guest's `reconnect` frame carries no display name, so the
    // persisted copy is the only source for it after a host refresh.
    const guestNames = session.guestNames;
    if (guestNames && typeof guestNames === "object" && !Array.isArray(guestNames)) {
      for (const [pidStr, name] of Object.entries(guestNames)) {
        if (typeof name === "string" && name) {
          this.guestNames.set(Number(pidStr), name);
        }
      }
    }
    for (const token of session.kickedTokens) this.kickedTokens.add(token);
    for (const pid of session.eliminatedSeats) {
      this.eliminatedSeats.add(pid);
    }
    this.gameStarted = session.gameStarted;
    this.nativeAiDriverFault = session.nativeAiDriverFault ?? null;

    // Every persisted guest is "disconnected" from the resumed host's
    // POV until they dial back in. Arming a grace window for each means
    // `handleReconnect` takes its existing valid path when a returning
    // guest sends their token — no special-case branch needed.
    // Skip the host seat (PlayerId 0) which is this adapter's owner.
    // Skip eliminated seats — already out, no grace needed.
    for (const pidStr of Object.keys(session.playerTokens)) {
      const pid = Number(pidStr);
      if (pid === 0) continue;
      if (this.eliminatedSeats.has(pid)) continue;
      this.armResumeGrace(pid);
    }
    // Mid-game resume: the game is paused until at least one guest
    // reconnects. Pre-game resume (lobby): state stays "running" since
    // `initializeGame` hasn't been called yet.
    if (this.nativeAiDriverFault !== null) {
      this.gameRunState = "terminal";
    } else if (this.gameStarted && this.disconnectedSeats.size > 0) {
      this.gameRunState = "paused-disconnect";
    }
  }

  /**
   * Pre-seed a persisted guest seat as disconnected on host resume, so a
   * returning guest's token takes `handleReconnect`'s existing valid path.
   * No grace timer is armed: consistent with the mid-game disconnect policy,
   * a player who hasn't returned is never auto-conceded. The seat is held
   * indefinitely (game paused) until the guest reconnects; the host may
   * explicitly concede or kick a seat that never comes back.
   */
  private armResumeGrace(pid: PlayerId): void {
    this.disconnectedSeats.set(pid, { disconnectedAt: Date.now(), timer: null });
  }

  /** Product elimination is a cache of the engine's public authority state.
   * Native resume corrects both stale directions while leaving token denial
   * independent, so a kicked live seat stays denied without being eliminated. */
  private reconcileEliminatedSeats(snapshot: EngineSnapshot): void {
    const authoritative = new Set(snapshot.state.eliminated_players ?? []);
    for (const pid of this.eliminatedSeats) {
      if (!authoritative.has(pid)) this.eliminatedSeats.delete(pid);
    }
    for (const pid of authoritative) this.eliminatedSeats.add(pid);
    for (const pid of this.playerTokens.keys()) {
      if (pid === 0) continue;
      if (authoritative.has(pid)) {
        const grace = this.disconnectedSeats.get(pid);
        if (grace?.timer !== null && grace?.timer !== undefined) clearTimeout(grace.timer);
        this.disconnectedSeats.delete(pid);
      } else if (!this.disconnectedSeats.has(pid) && !this.guestSessions.has(pid)) {
        this.armResumeGrace(pid);
      }
    }
    // The run state was chosen from the persisted membership, before this
    // correction. Bring it in step with the corrected one: an away seat pauses
    // the game, and the last stale one leaving releases the pause it caused.
    // Submissions enforce this value, so a stale pause would refuse every
    // action indefinitely and a stale "running" would admit actions while a
    // seat is away. A terminal or manually paused game is never touched.
    if (this.disconnectedSeats.size > 0) {
      if (this.gameRunState === "running") this.gameRunState = "paused-disconnect";
    } else if (this.gameRunState === "paused-disconnect") {
      this.resumeIfUnblocked();
    }
  }

  /**
   * Build a persisted snapshot from the current in-memory adapter
   * state. Returns null when persistence isn't configured (tests,
   * ephemeral hosts) so save-hooks can short-circuit cleanly.
   */
  private buildPersistedSession(): PersistedP2PHostSession | null {
    if (!this.gameId || !this.roomCode) return null;
    const nativeSession = this.nativeBridge?.persistence();
    const playerTokens: Record<number, string> = {};
    for (const [pid, token] of this.playerTokens.entries()) {
      playerTokens[pid] = token;
    }
    const guestDecks: Record<number, unknown> = {};
    for (const [pid, deck] of this.guestDecks.entries()) {
      guestDecks[pid] = deck;
    }
    const aiDecks: Record<number, unknown> = {};
    for (const [pid, deck] of this.aiDecks.entries()) {
      aiDecks[pid] = deck;
    }
    const guestNames: Record<number, string> = {};
    for (const [pid, name] of this.guestNames.entries()) {
      guestNames[pid] = name;
    }
    return {
      gameId: this.gameId,
      roomCode: this.roomCode,
      sessionKey: this.sessionKey,
      brokerGameCode: this.brokerGameCode,
      useBroker: this.broker !== undefined,
      playerTokens,
      guestDecks,
      guestNames,
      aiDecks,
      kickedTokens: [...this.kickedTokens],
      eliminatedSeats: [...this.eliminatedSeats],
      playerCount: this.playerCount,
      formatConfig: this.formatConfig,
      matchConfig: this.matchConfig,
      hostDeckData: this.hostDeckData,
      gameStarted: this.gameStarted,
      seatState: this.pregameSeatState,
      ...(this.nativeAiDriverFault ? { nativeAiDriverFault: this.nativeAiDriverFault } : {}),
      ...(nativeSession ? { nativeSession } : {}),
    };
  }

  getPlayerSlots(): PlayerSlot[] {
    return this.pregameSeatView.seats.map((kind, playerId) => ({
      playerId,
      kind,
      teamInfo: this.pregameSeatView.teamInfo?.[playerId] ?? undefined,
      name: this.displayNameForSeat(playerId, kind),
    }));
  }

  usesNativeEngine(): boolean {
    return this.nativeBridge !== null;
  }

  private displayNameForSeat(playerId: number, kind: SeatKind): string {
    if (playerId === 0) {
      return this.hostDisplayName ?? "Host";
    }
    if (kind.type === "Ai") {
      // Use the AI's commander as their persona — matches the feel of offline
      // play where opponents are recognizable rather than anonymous "AI"
      // labels. Strip everything after the first comma so
      // "Otrimi, the Ever-Playful" → "Otrimi". Falls back to the difficulty
      // label if the seat has no resolved commander yet (transient pregame
      // state before `applySeatMutation` lands the deck).
      const deck = this.aiDecks.get(playerId);
      const commander = deck?.commander?.[0];
      if (commander) {
        const shortName = commander.split(",")[0].trim();
        return `${shortName} (AI · ${kind.data.difficulty})`;
      }
      return `AI (${kind.data.difficulty})`;
    }
    // Human guest. Prefer the displayName the guest sent over the wire; fall
    // back to their commander short name (mirroring the AI seat). The guest's
    // displayName is optional and absent for users who never set one in the
    // multiplayer store — without this fallback, the host's UI labels the
    // seat "Opp N" while every other client (which receives the same name
    // map) sees nothing missing for their own perspective.
    const stored = this.guestNames.get(playerId);
    if (stored) return stored;
    const guestCommander = this.guestDecks.get(playerId)?.commander?.[0];
    if (guestCommander) return guestCommander.split(",")[0].trim();
    return "";
  }

  /**
   * Write the current adapter state to disk. Fire-and-forget:
   * lifecycle event handlers don't block on IDB. Failures are logged
   * but never thrown — losing a write means a slightly stale resume
   * snapshot, not a crash.
   */
  private saveSession(): void {
    if (!this.ownsAuthority()) return;
    if (!this.gameId) return;
    if (this.nativeTerminalPersistenceClearing || this.nativeTerminalPersistenceCleared) return;
    const snapshot = this.buildPersistedSession();
    if (!snapshot) return;
    void saveP2PHostSession(this.gameId, snapshot);
  }

  /** Persist the host authority as the engine's opaque trusted envelope. */
  private async persistAuthoritativeState(): Promise<void> {
    if (!this.ownsAuthority()) return;
    if (this.nativeBridge) return;
    if (!this.gameId) return;
    try {
      const json = await this.wasm.exportPersistenceState();
      await saveGame(this.gameId, JSON.parse(json) as PersistedGameState);
    } catch (err) {
      console.warn("[P2PHost] trusted state export failed:", err);
    }
  }

  /**
   * Resume is a publish barrier, unlike ordinary best-effort autosave. The
   * exact post-automation state must reach durable storage before a guest can
   * reconnect to it.
   */
  private async persistResumedAuthority(): Promise<void> {
    if (!this.ownsAuthority() || this.nativeBridge || !this.gameId) {
      throw new AdapterError("P2P_ERROR", "Resumed host has no durable WASM authority", false);
    }
    const json = await this.wasm.exportPersistenceState();
    await saveResumableGameStrict(this.gameId, JSON.parse(json) as PersistedGameState);
  }

  /** Abort an unpublished resumed engine so no reconnect can observe volatile
   * state. Also stops the redelivery sweep: it is armed in `initializeInner`
   * before the resume barrier, and only `dispose()` cleared it, so reaching
   * this path left a 5 s interval firing for the life of the page. A resume
   * that fails before this path is reached still relies on `dispose()`. */
  private async abortUnpublishedResume(): Promise<void> {
    const owner = this.wasmHostOwner;
    this.wasmHostOwner = null;
    this.resumedAutomation = null;
    this.unsubscribeHostConnections();
    this.disposed = true;
    if (this.redeliveryTimer !== null) {
      clearInterval(this.redeliveryTimer);
      this.redeliveryTimer = null;
    }
    if (sharedEngineHost === this.engineClaim) sharedEngineHost = null;
    if (owner) await this.wasm.releaseHostSession(true, owner);
    releaseP2PHostLease(this.authority);
    try {
      this.hostPeer.destroy();
    } catch {
      /* best-effort transport teardown after the durable barrier failed */
    }
  }

  /**
   * The persisted lease is checked at every authority boundary. This is a
   * fence, not advisory bookkeeping: a resumed host with the same session key
   * has already superseded this adapter and its delayed work must become inert.
   */
  private ownsAuthority(): boolean {
    const owns = ownsP2PHostLease(this.authority);
    if (!owns) traceAdapter("Host", "lease-fenced", { sessionKey: this.sessionKey });
    return owns;
  }

  /** Engine entry-point guard — see `hostDisposedError`. */
  private assertNotDisposed(): void {
    if (this.disposed) throw hostDisposedError();
  }

  /**
   * Abandon an in-flight init/start that a `dispose()` overtook, leaving
   * nothing owning the engine.
   *
   * `claimed` must be true whenever a state-installing call
   * (`initializeMultiplayerHostGame`, `resumeMultiplayerHostState`) already
   * resolved, and false otherwise — including on every rejection, since the
   * engine claims itself only on a successful install and a failed call leaves
   * it untouched. Get it wrong in the `true` direction and a shared engine is
   * reset out from under whoever does own it; wrong in the `false` direction
   * and the flag plus the ownerless game it installed sit on the shared engine
   * forever — `clear_game_state` does not clear the multiplayer flag and local
   * games never touch it, so the residue would refuse undo in every later local
   * game and refuse every hosted resume. The release is routed through
   * `releaseHostSession` rather than a direct `setMultiplayerMode(false)`
   * because the private/desktop adapter has already been disposed by then and
   * would throw `assertInitialized`, turning a clean bail into an unhandled
   * rejection.
   */
  private async bailDisposed(
    claimed: boolean,
    during: string,
    owner: HostSessionOwner | null = this.wasmHostOwner,
  ): Promise<never> {
    if (owner) await this.wasm.releaseHostSession(claimed, owner);
    else await this.wasm.releaseHostSession(claimed);
    if (owner === this.wasmHostOwner) this.wasmHostOwner = null;
    throw new AdapterError("P2P_ERROR", `Host session disposed during ${during}`, true);
  }

  private send(session: PeerSession, message: P2PMessage): Promise<boolean> {
    if (!this.ownsAuthority()) return Promise.resolve(false);
    return session.send({ ...message, authority: this.authority });
  }

  /** Keep state fan-out, terminal delivery, and reconnect acknowledgement /
   * promotion in one ordered critical section. PeerJS reconnects arrive
   * independently of both the native revision stream and browser WASM action
   * fan-out; without this fence either path can skip a pending session while
   * the reconnect acknowledges an older snapshot. */
  private enqueueDelivery<T>(operation: () => Promise<T>): Promise<T> {
    const result = this.deliveryQueue.then(operation, operation);
    this.deliveryQueue = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }

  /**
   * Single authority for stamping ordinary browser-host gameplay mutations.
   * The operation is awaited first, so rejected/stale/error calls never move
   * the browser-only coherence fence.
   */
  private async applyBrowserMutation(
    operation: () => Promise<SubmitResult>,
  ): Promise<BrowserTransition> {
    const browserMutation = this.nativeBridge === null;
    const transition = this.stampBrowserMutation(await operation(), browserMutation);
    // A concession that ended the vote unapproved is heard before its state;
    // an approval follows its state (`broadcastStateUpdateInner`).
    const takeback = transition.result.takeback;
    if (takeback && takeback.resolution !== "approved") {
      await this.enqueueDelivery(async () => this.announceTakeback(resolvedTakebackEvent(takeback)));
    }
    return transition;
  }

  /** Stamp a successful browser-WASM gameplay mutation after the engine accepts it. */
  private stampBrowserMutation(
    result: SubmitResult,
    browserMutation = this.nativeBridge === null,
  ): BrowserTransition {
    if (browserMutation) this.browserMutationGeneration += 1;
    return { result, generation: this.browserMutationGeneration };
  }

  /**
   * Project one accepted transition for one recipient. If another browser
   * mutation overtakes the projection, send a current state-only snapshot so
   * stale events and logs cannot be paired with newer state.
   */
  private async projectTransitionForViewer(
    viewer: PlayerId,
    events: GameEvent[],
    expectedGeneration: number,
  ): Promise<ProjectedBrowserTransition> {
    if (expectedGeneration !== this.browserMutationGeneration) {
      return { snapshot: await this.wasm.getViewerSnapshot(viewer), events: [], fresh: false };
    }
    const transition = await this.wasm.getViewerTransitionSnapshot(viewer, events);
    if (expectedGeneration === this.browserMutationGeneration) {
      return { snapshot: transition, events: transition.events, fresh: true };
    }
    return { snapshot: await this.wasm.getViewerSnapshot(viewer), events: [], fresh: false };
  }

  private rejectSuperseded(session: PeerSession): void {
    void session.send({ type: "reconnect_rejected", reason: "Host session superseded" });
    session.close("Host session superseded");
  }

  /**
   * Resolves the guest-deck gate in `initializeGame` so the engine starts
   * with whatever guests have connected so far. For 2p rooms this is
   * functionally "start now that the one guest is here"; for 3-4p rooms
   * it starts with fewer seats than configured — callers are responsible
   * for their own AI-seat-synthesis follow-up.
   *
   * Does NOT itself talk to the broker — the unregister call cascades
   * through `initializeGame`, which is the single authority for the
   * broker-side lifecycle (per CLAUDE.md's "single authority" rule).
   */
  startNow(): void {
    this.allowPartialStart = true;
    const resolvers = this.guestDeckResolvers.splice(0);
    for (const r of resolvers) r();
  }

  private enqueuePregameOp<T>(work: () => Promise<T>): Promise<T> {
    const next = this.pregameOpQueue.then(work, work);
    this.pregameOpQueue = next.then(() => undefined, () => undefined);
    return next;
  }

  private firstWaitingSeat(): PlayerId | null {
    for (let seat = 1; seat < this.pregameSeatState.seats.length; seat++) {
      if (this.pregameSeatState.seats[seat]?.type === "WaitingHuman") {
        return seat;
      }
    }
    return null;
  }

  private remapSeatMap<T>(source: Map<PlayerId, T>, remapping: Array<[number, number]>): Map<PlayerId, T> {
    const remapped = new Map<PlayerId, T>();
    for (const [pid, value] of source.entries()) {
      const mapped = remapping.find(([oldPid]) => oldPid === pid)?.[1] ?? pid;
      remapped.set(mapped, value);
    }
    return remapped;
  }

  private remapSeatSet(source: Set<PlayerId>, remapping: Array<[number, number]>): Set<PlayerId> {
    const remapped = new Set<PlayerId>();
    for (const pid of source.values()) {
      remapped.add(remapping.find(([oldPid]) => oldPid === pid)?.[1] ?? pid);
    }
    return remapped;
  }

  private broadcastSeatSnapshot(): void {
    if (!this.ownsAuthority()) return;
    for (const session of this.guestSessions.values()) {
      void this.send(session, { type: "seat_snapshot", view: this.pregameSeatView });
    }
    this.emit({ type: "playerSlotsUpdated", slots: this.getPlayerSlots() });
  }

  private async refreshPregameSeatView(): Promise<void> {
    this.pregameSeatView = await this.wasm.projectSeatView(
      JSON.stringify(this.pregameSeatState),
    ) as SeatView;
  }

  private playerNamesForSeats(): Record<number, string> {
    const names: Record<number, string> = {};
    for (const [playerId, kind] of this.pregameSeatState.seats.entries()) {
      const name = this.displayNameForSeat(playerId, kind);
      if (name) names[playerId] = name;
    }
    return names;
  }

  private syncLobbyMetadata(consumedReservationTokens: string[] = []): void {
    if (!this.ownsAuthority()) return;
    const currentPlayers = occupiedSeatCount(this.pregameSeatState);
    const maxPlayers = this.pregameSeatState.seats.length;
    this.emit({ type: "lobbyProgress", joined: currentPlayers, total: maxPlayers });
    if (this.broker && this.brokerGameCode) {
      this.broker.updateMetadata(
        this.brokerGameCode,
        currentPlayers,
        maxPlayers,
        consumedReservationTokens,
      );
    }
  }

  async applySeatMutation(mutation: SeatMutation): Promise<void> {
    await this.enqueuePregameOp(async () => {
      this.assertNotDisposed();
      if (!this.ownsAuthority()) return;
      if (this.gameStarted) {
        throw new AdapterError("P2P_ERROR", "Pregame seats can no longer be edited", false);
      }
      if (mutation.type === "Start") {
        throw new AdapterError("P2P_ERROR", "Use startPregameGame() for Start mutations", false);
      }

      const result = await this.wasm.applySeatMutation(
        JSON.stringify(this.pregameSeatState),
        JSON.stringify(mutation),
      ) as SeatMutationResult;

      for (const token of result.delta.invalidatedTokens) {
        for (const [pid, seatToken] of this.playerTokens.entries()) {
          if (seatToken !== token) continue;
          const session = this.guestSessions.get(pid);
          if (session) {
            void this.send(session, { type: "kick", reason: "Removed from the room by the host" });
            try {
              session.close("Removed by host");
            } catch {
              /* best-effort */
            }
          }
          this.guestSessions.delete(pid);
          this.nativeBridge?.detachGuest(pid);
          this.playerTokens.delete(pid);
          this.guestDecks.delete(pid);
          this.guestNames.delete(pid);
          break;
        }
      }

      for (const seatIndex of result.delta.removedAi) {
        this.aiDecks.delete(seatIndex);
      }
      for (const [seatIndex, _difficulty, deck] of result.delta.newAi) {
        // Rust SeatDelta now carries name-only PlayerDeckList — match the
        // shape with a type guard, no cast.
        if (isDeckListPlayerShape(deck)) {
          this.aiDecks.set(seatIndex, deck);
        }
      }

      if (result.delta.renumbering) {
        const { remapping } = result.delta.renumbering;
        this.guestSessions = this.remapSeatMap(this.guestSessions, remapping);
        this.guestDecks = this.remapSeatMap(this.guestDecks, remapping);
        this.aiDecks = this.remapSeatMap(this.aiDecks, remapping);
        this.playerTokens = this.remapSeatMap(this.playerTokens, remapping);
        this.guestNames = this.remapSeatMap(this.guestNames, remapping);
        this.disconnectedSeats = this.remapSeatMap(this.disconnectedSeats, remapping);
        this.eliminatedSeats = this.remapSeatSet(this.eliminatedSeats, remapping);
      }

      this.pregameSeatState = result.state;
      if (this.nativeBridge) {
        await this.nativeBridge.applySeatMutation(mutation);
      }
      await this.refreshPregameSeatView();
      this.saveSession();
      for (const session of this.guestSessions.values()) {
        void this.send(session, { type: "seat_mutate", mutation });
      }
      this.broadcastSeatSnapshot();
      this.syncLobbyMetadata();

      if (this.firstWaitingSeat() === null) {
        this.emit({ type: "roomFull" });
      }
    });
  }

  private async runAiLoop(): Promise<void> {
    if (!this.ownsAuthority()) return;
    if (this.nativeBridge) return;
    if (!this.gameStarted) return;

    let staleRetries = 0;
    for (;;) {
      if (!this.ownsAuthority()) return;
      // A disposed host must stop driving the engine. With a private worker
      // the next call threw `assertInitialized` and ended the loop; a shared
      // worker would happily keep applying AI actions to whatever game is
      // there now.
      if (this.disposed) return;
      if (this.gameRunState !== "running") return;
      const state = await this.wasm.getState();
      if (!state || typeof state !== "object" || !("waiting_for" in state)) {
        return;
      }
      const waitingFor = state.waiting_for;
      if (!waitingFor || typeof waitingFor !== "object") {
        return;
      }
      if (!("data" in waitingFor) || !waitingFor.data) {
        return;
      }
      const actor = aiActorFromWaitingFor(
        waitingFor as WaitingFor,
        this.pregameSeatState.seats,
        state.priority_player,
      );
      if (actor == null) {
        return;
      }
      const aiSeat = this.pregameSeatState.seats[actor];
      if (!aiSeat || aiSeat.type !== "Ai") {
        return;
      }
      const proposal = await this.wasm.getAiActionProposal(aiSeat.data.difficulty, actor);
      if (!proposal) {
        return;
      }
      const outcome = await this.wasm.submitAiActionProposal(proposal);
      if (outcome.status === "stale") {
        staleRetries += 1;
        if (staleRetries > MAX_AI_PROPOSAL_STALE_RETRIES) {
          throw new AdapterError(
            "P2P_ERROR",
            `AI proposal repeatedly stale: ${outcome.reason}`,
            true,
          );
        }
        continue;
      }
      if (outcome.status === "rejected") {
        throw actionRejectionError(outcome.rejection);
      }
      staleRetries = 0;
      const transition = this.stampBrowserMutation(outcome.result);
      await this.publishHostSnapshot(transition.result);
      await this.broadcastStateUpdate(transition);
      void this.persistAuthoritativeState();
    }
  }

  onEvent(listener: P2PAdapterEventListener): () => void {
    this.listeners.push(listener);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener);
    };
  }

  private emit(event: P2PAdapterEvent): void {
    for (const listener of this.listeners) {
      listener(event);
    }
  }

  private resetPregameReady(): void {
    this.pregameReady = new Promise((resolve, reject) => {
      this.resolvePregameReady = resolve;
      this.rejectPregameReady = reject;
    });
    // Initialization can fail before a guest arrives to await this gate. Keep
    // that rejection observable to a queued guest while preventing it from
    // becoming an unhandled rejection when no guest exists yet.
    void this.pregameReady.catch(() => {});
  }

  private unsubscribeHostConnections(): void {
    this.hostConnectionUnsub?.();
    this.hostConnectionUnsub = null;
  }

  async initialize(): Promise<void> {
    this.assertNotDisposed();
    if (this.initialized) return;
    if (this.initPromise) return this.initPromise;
    this.resetPregameReady();
    const pending = this.initializeInner();
    this.initPromise = pending;
    pending.catch(() => {
      if (this.initPromise === pending) this.initPromise = null;
    });
    return pending;
  }

  private async initializeInner(): Promise<void> {
    traceAdapter("Host", "initialize-start", { isResume: this.isResume });
    // Subscribe SYNCHRONOUSLY before any `await`. `hostRoom()` buffers
    // inbound guest connections that arrived between peer-open and the
    // first `onGuestConnected` subscribe, and flushes them into this
    // handler on subscribe — so no guest is dropped, even if the broker
    // registration + adapter construction held this call off for hundreds
    // of ms while `wasm.initialize()` was cold-loading.
    this.hostConnectionUnsub = this.onGuestConnected((conn) => {
      if (!this.ownsAuthority()) {
        const session = createPeerSession(conn, {});
        this.rejectSuperseded(session);
        return;
      }
      traceAdapter("Host", "handle-connection-event", { connOpen: conn.open });
      this.handleNewConnection(conn);
    });

    try {
      await this.wasm.initialize();
      if (this.nativeBridge) {
        try {
          await this.nativeBridge.initializeHost([]);
          if (this.isResume && this.gameStarted) {
            const snapshot = await this.nativeBridge.getSnapshot();
            this.reconcileEliminatedSeats(snapshot);
            for (const [playerId, report] of this.nativeBridge.seatReports()) {
              const revision = seatReportRevision(report);
              this.recordNativeSeatOutcome(playerId, revision, report);
              this.authoritativeRevision = Math.max(this.authoritativeRevision, revision);
            }
            this.resumeGameState = null;
          } else {
            this.nativeTerminalAckReady = true;
          }
          this.saveSession();
        } catch (err) {
          if (this.isResume) {
            // A resumed native game already has phase-server as its sole
            // authority. Falling back to a fresh WASM engine here would fork
            // the game from the state guests are reconnecting to.
            throw err;
          }
          // No P2P guest has been accepted yet: abandon the partial local
          // server session and retain the established WASM authority path.
          // Once Start has succeeded, switching authority would diverge.
          console.warn("[P2PHost] native engine setup failed; using WASM host", err);
          await this.nativeBridge.abandon().catch(() => {
            /* best-effort cleanup before local fallback */
          });
          this.nativeBridge.dispose();
          this.nativeBridge = null;
          this.attachAuthorityDiagnostics();
        }
      }
      // A teardown may have landed while `wasm.initialize()` (and the native
      // handshake above) were in flight. Bail before installing anything:
      // nothing has been claimed yet, so there is nothing to undo.
      if (this.disposed) await this.bailDisposed(false, "initialization");
      this.startRedeliverySweep();
      if (this.nativeBridge && this.isResume && this.gameStarted) {
        const snapshot = await this.nativeBridge.getSnapshot();
        if (snapshot.state.waiting_for.type === "GameOver") {
          this.gameRunState = "terminal";
          this.gameStarted = true;
          this.pregameSeatState.gameStarted = true;
          const terminalResume = await this.nativeBridge.terminalResume();
          if (this.resumeTerminalResult) {
            const retained = this.resumeTerminalResult;
            const finalView = terminalResume?.snapshot.state;
            const finalRevision = terminalResume?.revision;
            const display = terminalResume?.display;
            if (
              !terminalResume
              || !isValidP2PTerminalResult(retained)
              || retained.key !== this.sessionKey
              || retained.recipient !== 0
              || retained.revision !== finalRevision
              || !display
              || retained.display.winner !== display.winner
              || retained.display.reason !== display.reason
              || !finalView
              || (await p2pFinalStateCommitment(finalView)) !== retained.finalStateCommitment
            ) {
              throw new AdapterError(
                "P2P_ERROR",
                "Retained P2P terminal result does not match Full final authority",
                false,
              );
            }
            this.terminalResult = retained;
            this.emit({ type: "terminalResult", result: retained });
          }
          if (!(await this.commitTerminalIfComplete(
            terminalResume?.snapshot ?? snapshot,
            terminalResume?.revision ?? this.authoritativeRevision,
          ))) {
            throw new AdapterError("P2P_ERROR", "Failed to retain resumed native terminal result", false);
          }
        }
        this.nativeTerminalAckReady = true;
        await this.tryAcknowledgeNativeTerminal();
      }
      // Resume path: load the persisted GameState with a fresh RNG seed
      // and atomic multiplayer-flag claim. `resumeMultiplayerHostState`
      // mirrors server-core's `from_persisted` pattern, and
      // `initializeMultiplayerHostGame` is its fresh-start sibling — both
      // refuse an engine that is already in use and claim the flag themselves
      // in the same call that installs the state. No client code sets the flag,
      // so an open lobby leaves zero engine footprint.
      if (this.isResume && this.resumeGameState && !this.nativeBridge) {
        const gameId = this.gameId;
        if (!gameId) {
          throw new AdapterError("P2P_ERROR", "Resumed host is missing its durable game id", false);
        }
        const owner = createHostSessionOwner();
        this.wasmHostOwner = owner;
        this.resumedAutomation = await this.wasm.resumeMultiplayerHostState(this.resumeGameState, owner);
        this.resumeGameState = null;
        // The engine now holds both this game's state and the multiplayer
        // flag. Its await window is the widest in the adapter (the full card
        // DB load happens inside), so re-check before recording the claim.
        if (this.disposed) await this.bailDisposed(true, "resume", owner);
        // A same-session resume may have superseded this host while the engine
        // was restoring the persisted state. The stale adapter must release
        // only the owner it captured for this attempt and must not publish,
        // persist, or clear the resumed game belonging to the live host.
        if (!this.ownsAuthority()) {
          await this.wasm.releaseHostSession(true, owner);
          if (this.wasmHostOwner === owner) this.wasmHostOwner = null;
          throw new AdapterError("P2P_ERROR", "Host session superseded", true);
        }
        sharedEngineHost = this.engineClaim;
        // Persist the post-automation authority before any reconnect can be
        // accepted or snapshot published. A terminal restore first creates its
        // durable terminal record, then drops stale resumable state instead.
        if (this.resumedAutomation.snapshot.state.waiting_for.type === "GameOver") {
          const committed = await this.commitTerminalIfComplete(
            this.resumedAutomation.snapshot,
            ++this.authoritativeRevision,
          );
          if (!committed) {
            throw new AdapterError("P2P_ERROR", "Failed to retain resumed terminal result", false);
          }
          await clearGame(gameId);
        } else {
          await this.persistResumedAuthority();
        }
        traceAdapter("Host", "initialize-resume", {
          tokens: this.playerTokens.size,
          gameStarted: this.gameStarted,
        });
      }
      this.resolvePregameReady();
    } catch (err) {
      if (this.isResume && sharedEngineHost === this.engineClaim) {
        await this.abortUnpublishedResume();
      } else if (this.isResume) {
        this.wasmHostOwner = null;
      }
      this.unsubscribeHostConnections();
      this.rejectPregameReady(err);
      throw err;
    }
    if (!this.gameStarted) {
      await this.enqueuePregameOp(async () => {
        await this.refreshPregameSeatView();
        this.broadcastSeatSnapshot();
        this.syncLobbyMetadata();
      });
    } else if (this.isResume) {
      // A refreshed host's store starts with no names, and `initializeGame`
      // (the fresh-start emitter) never runs on resume. Re-announce the seat
      // names here so the host's own UI labels its guests again.
      this.emit({ type: "playerIdentity", playerId: 0, playerNames: this.playerNamesForSeats() });
    }
    traceAdapter("Host", "initialize-complete", {});
    this.initialized = true;
  }

  private publishPlayerLatencies(): void {
    if (!this.ownsAuthority()) return;
    const latencies: Record<number, number | null> = { 0: 0 };
    for (const [pid, session] of this.guestSessions) {
      latencies[pid] = this.sessionLatencies.get(session) ?? null;
    }
    this.emit({ type: "playerLatencies", latencies });
    for (const session of this.guestSessions.values()) {
      void this.send(session, { type: "player_latencies", latencies });
    }
  }

  private handleNewConnection(conn: TransportConnection): void {
    if (!this.ownsAuthority()) {
      const session = createPeerSession(conn, {});
      this.rejectSuperseded(session);
      return;
    }
    traceAdapter("Host", "handle-new-connection", { connOpen: conn.open });
    // Reconnect path: the first message determines whether this is a fresh
    // join or a reconnect. We attach a one-shot pre-handler to peek at the
    // first message before wrapping in a PeerSession with full handlers.
    let identified = false;
    const session = createPeerSession(conn, {
      onStateAck: (msg) => {
        if (!identified) return false;
        this.handleGuestStateAck(session, msg);
        return true;
      },
      onLatency: (latencyMs) => {
        if (![...this.guestSessions.values()].includes(session)) return;
        this.sessionLatencies.set(session, latencyMs);
        this.publishPlayerLatencies();
      },
      onSessionEnd: () => {
        this.closedPregameSessions.add(session);
        // Find which seat this session belonged to (if any) and route to the
        // appropriate disconnect handler.
        for (const [pid, s] of this.guestSessions.entries()) {
          if (s === session) {
            this.handleGuestDisconnect(pid);
            return;
          }
        }
        this.clearPendingReconnectReservation(session);
      },
      onUndeliverableFrame: (cause) => {
        // `identified` flips only inside the one-shot `onMessage` below, which
        // runs only on a decodable frame — so the only discriminator between a
        // token-bearing guest and a tokenless one is inside the frame this host
        // could not read. Both get the terminal answer rather than leaving the
        // tokenless one stranded. Send-then-close mirrors the invalid-first-
        // message arm below.
        if (identified) return;
        void session.send({
          type: "reconnect_rejected",
          reason: `Undecodable first message (${cause})`,
          reasonCode: "first_message_invalid",
        });
        session.close("Undecodable first message");
      },
    });

    const unsub = session.onMessage((msg) => {
      if (identified) return;
      identified = true;
      unsub();

      if (msg.type !== "guest_deck" && msg.type !== "reconnect") {
        traceAdapter("Host", "first-message", { type: msg.type });
        void session.send({
          type: "reconnect_rejected",
          reason: "Expected guest_deck or reconnect as first message",
          reasonCode: "first_message_invalid",
        });
        session.close("Protocol violation");
        return;
      }

      // v28 makes the first-contact version mandatory. Reject before a seat,
      // deck, token, or authority is adopted: an unstamped peer cannot decode
      // the structured action-rejection contract safely.
      const guestVersion: number | undefined = msg.wireProtocolVersion;
      if (guestVersion !== WIRE_PROTOCOL_VERSION) {
        // Reaches the refused guest's user RAW, the same way the guest-side
        // reason at `handleHostMessage` does. `i18n/README.md` asks for `t()`
        // on frontend-authored strings; this one is new text on that raw path
        // and is written unkeyed deliberately, to match the incumbent
        // wording rather than split the pair. Noted, not fixed: the raw-string
        // path is pre-existing and out of scope here.
        const reason = guestVersion === undefined
          ? `Wire protocol version required: this host speaks v${WIRE_PROTOCOL_VERSION}. Refresh both windows.`
          : `Wire protocol mismatch: guest sent v${guestVersion}, this host speaks v${WIRE_PROTOCOL_VERSION}. Refresh both windows.`;
        traceAdapter("Host", "first-message-version-mismatch", { type: msg.type, guestVersion });
        void session.send({
          type: "reconnect_rejected",
          reason,
          reasonCode: guestVersion === undefined ? "wire_protocol_version_required" : "wire_protocol_mismatch",
          hostWireProtocolVersion: WIRE_PROTOCOL_VERSION,
          guestWireProtocolVersion: guestVersion,
        });
        session.close("Wire protocol mismatch");
        return;
      }

      if (msg.type === "reconnect") {
        traceAdapter("Host", "first-message", { type: msg.type });
        if (msg.authority !== undefined && !isP2PAuthorityStamp(msg.authority)) {
          void session.send({
            type: "reconnect_rejected",
            reason: "Malformed P2P authority",
            reasonCode: "malformed_authority",
          });
          session.close("Malformed P2P authority");
          return;
        }
        void this.handleReconnect(session, msg.playerToken, msg.sessionKey, msg.authority);
      } else if (msg.type === "guest_deck") {
        traceAdapter("Host", "first-message", { type: msg.type });
        void this.handleNewGuest(
          session,
          msg.deckData,
          msg.displayName,
          msg.reservationToken,
        ).catch((err) => {
          traceAdapter("Host", "new-guest-error", {
            error: err instanceof Error ? err.message : String(err),
          });
          void this.send(session, { type: "kick", reason: "Host failed to add player" });
          session.close("Host failed to add player");
        });
      }
    });
  }

  private async handleNewGuest(
    session: PeerSession,
    deckData: unknown,
    displayName?: string,
    reservationToken?: string,
  ): Promise<void> {
    await this.enqueuePregameOp(async () => {
      if (!this.ownsAuthority()) {
        this.rejectSuperseded(session);
        return;
      }
      await this.pregameReady;
      if (!this.ownsAuthority()) {
        this.rejectSuperseded(session);
        return;
      }
      if (this.closedPregameSessions.has(session)) return;
      if (this.gameStarted) {
        void this.send(session, { type: "kick", reason: "Game already in progress" });
        session.close("Game in progress");
        return;
      }
      const pid = this.firstWaitingSeat();
      if (pid === null) {
        void this.send(session, { type: "kick", reason: "Lobby full" });
        session.close("Lobby full");
        return;
      }

      // `deckData` is typed `unknown` at the wire boundary (see
      // network/protocol.ts). The guest sends a `DeckListPayload`-shaped object
      // and we only need its `.player` slot here. If a malformed wire payload
      // arrives, fall through to an empty deck — the engine's
      // `deck_pools.is_empty()` invariant will reject it loudly at game start.
      const guestDeckRaw =
        deckData !== null && typeof deckData === "object" && "player" in deckData
          ? (deckData as { player: unknown }).player
          : undefined;
      const guestDeck: DeckListPayload["player"] = isDeckListPlayerShape(
        guestDeckRaw,
      )
        ? guestDeckRaw
        : { main_deck: [], sideboard: [], commander: [], planar_deck: [], scheme_deck: [] };

      if (this.nativeBridge) {
        try {
          await this.nativeBridge.attachGuest(pid, guestDeck, displayName ?? `Player ${pid + 1}`);
        } catch (err) {
          // Still pre-start, so a native per-seat attach failure may safely
          // fall back to the existing WASM authority without exposing a mixed
          // authority game to any PeerJS guest.
          console.warn("[P2PHost] native guest attachment failed; using WASM host", err);
          await this.nativeBridge.abandon().catch(() => {
            /* best-effort cleanup before local fallback */
          });
          this.nativeBridge.dispose();
          this.nativeBridge = null;
          this.attachAuthorityDiagnostics();
        }
      }
      if (!this.ownsAuthority()) {
        this.rejectSuperseded(session);
        return;
      }
      if (this.closedPregameSessions.has(session)) {
        // The native attach can outlive the PeerJS channel. Release the
        // server-side seat before returning, and never publish a local guest
        // session for a connection that already closed.
        if (this.ownsAuthority()) {
          await this.releaseNativePregameSeat(pid, "disconnect during guest attachment");
        }
        return;
      }

      const token = crypto.randomUUID();
      this.playerTokens.set(pid, token);
      this.guestSessions.set(pid, session);
      this.guestDecks.set(pid, guestDeck);
      this.publishPlayerLatencies();
      if (displayName) this.guestNames.set(pid, displayName);
      this.pregameSeatState.seats[pid] = { type: "JoinedHuman" };
      this.pregameSeatState.tokens[pid] = token;
      await this.refreshPregameSeatView();
      this.saveSession();

      session.onMessage((msg) => this.handleGuestMessage(session, msg));

      this.broadcastSeatSnapshot();
      this.syncLobbyMetadata(reservationToken ? [reservationToken] : []);

      if (this.formatConfig) {
        void this.validateGuestDeck(pid, guestDeck);
      }

      if (this.firstWaitingSeat() === null) {
        this.emit({ type: "roomFull" });
      }
    });
  }

  private async validateGuestDeck(
    pid: PlayerId,
    deck: DeckListPayload["player"],
  ): Promise<void> {
    await this.enqueuePregameOp(async () => {
      if (!this.ownsAuthority()) return;
      if (this.gameStarted) return;
      if (this.pregameSeatState.seats[pid]?.type !== "JoinedHuman") return;

      try {
        // This is a SECURITY GATE, not a UI hint: a guest whose deck fails is
        // kicked. It therefore uses the dedicated `evaluateDeckFormatGate`,
        // which always returns a definite verdict, and never the shared
        // `checkDeckCompatibility`. The shared one deliberately answers "no
        // opinion" (`selected_format_compatible: null`) for a Custom format —
        // correct for the lobby's legality chip, but here it would read as
        // "not false", silently admitting every Custom-format guest deck and
        // disabling this check for exactly the case it cannot evaluate.
        //
        // Still routed through the WORKER engine's already-resident card DB
        // (the adapter self-ensures it), not the main-thread `engineRuntime`
        // instance: that would parse a SECOND full ~93 MB card DB into the
        // page, doubling host footprint past iOS Safari's per-tab memory
        // ceiling and silently OOM-reloading the host tab.
        const result = await this.wasm.evaluateDeckFormatGate({
          main_deck: deck.main_deck,
          sideboard: deck.sideboard,
          commander: deck.commander ?? [],
          companion: deck.companion ?? [],
          signature_spell: deck.signature_spell ?? [],
          // CR 903.13f(3): `commander_draft_partner_grant` computes the
          // Commander Masters partner grant from exactly this field, so a
          // request that omits it REJECTS a rules-legal two-commander deck and
          // this gate kicks the guest. The host's own value travels as-is: no
          // `?? []`, which would assert "the draft contained zero sets" where
          // the host already knows the answer. (The engine reads absent, `null`
          // and `[]` identically, so this is a contract-vocabulary choice, not
          // a rules one.)
          draft_set_codes: (this.hostDeckData as DeckListPayload).draft_set_codes,
          selected_format: this.formatConfig!.format,
        }) as { compatible: boolean; reasons: string[] };

        if (!this.ownsAuthority()) return;
        if (this.gameStarted) return;
        if (!result.compatible) {
          const reason = result.reasons[0]
            ?? `Deck is not legal in ${this.formatConfig!.format}.`;
          const session = this.guestSessions.get(pid);
          if (session) {
            void this.send(session, { type: "kick", reason: `Deck rejected: ${reason}`, format: this.formatConfig!.format });
            session.close("Deck validation failed");
          }
          await this.releaseNativePregameSeat(pid, "deck rejection");
          this.guestSessions.delete(pid);
          this.playerTokens.delete(pid);
          this.guestDecks.delete(pid);
          this.guestNames.delete(pid);
          this.pregameSeatState.seats[pid] = { type: "WaitingHuman" };
          this.pregameSeatState.tokens[pid] = "";
          await this.refreshPregameSeatView();
          this.saveSession();
          this.broadcastSeatSnapshot();
          this.syncLobbyMetadata();
        }
      } catch (err) {
        traceAdapter("Host", "guest-deck-validation-error", {
          pid,
          error: err instanceof Error ? err.message : String(err),
        });
      }
    });
  }

  /** Keep the native pregame reducer aligned whenever a P2P human seat is
   * released before start. A failed sync is still safe to fall back because
   * no authoritative game state exists yet. */
  private async releaseNativePregameSeat(pid: PlayerId, reason: string): Promise<void> {
    if (!this.nativeBridge) return;
    try {
      await this.nativeBridge.applySeatMutation({
        type: "SetKind",
        data: { seatIndex: pid, kind: { type: "WaitingHuman" } },
      });
      this.nativeBridge.detachGuest(pid);
    } catch (err) {
      console.warn(`[P2PHost] native pregame ${reason} sync failed; using WASM host`, err);
      await this.nativeBridge.abandon().catch(() => {
        /* best-effort cleanup before local fallback */
      });
      this.nativeBridge.dispose();
      this.nativeBridge = null;
      this.attachAuthorityDiagnostics();
    }
  }

  async initializeGame(): Promise<SubmitResult> {
    return this.startPregameGame();
  }

  async startPregameGame(): Promise<SubmitResult> {
    return this.enqueuePregameOp(() => this.startPregameGameInner());
  }

  private async startPregameGameInner(): Promise<SubmitResult> {
      this.assertNotDisposed();
      if (!this.ownsAuthority()) {
        throw new AdapterError("P2P_ERROR", "Host session superseded", true);
      }
      if (this.gameStarted) {
        return { events: [] };
      }
      const allowPartialStart = this.allowPartialStart;
      this.allowPartialStart = false;
      const hasWaitingSeats = this.pregameSeatState.seats.some((seat) => seat.type === "WaitingHuman");
      if (hasWaitingSeats && !allowPartialStart) {
        throw new AdapterError("P2P_ERROR", "Fill or remove all open seats before starting", false);
      }

      const hostDeck = this.hostDeckData as DeckListPayload;
      const orderedOpponents: DeckListPayload["player"][] = [];
      const orderedDifficulties: string[] = [];
      for (let seat = 1; seat < this.pregameSeatState.seats.length; seat++) {
        const kind = this.pregameSeatState.seats[seat];
        if (kind.type === "JoinedHuman") {
          const deck = this.guestDecks.get(seat);
          if (!deck) {
            throw new AdapterError("P2P_ERROR", `Seat ${seat} has no submitted deck`, false);
          }
          orderedOpponents.push(deck);
          orderedDifficulties.push("");
          continue;
        }
        if (kind.type === "Ai") {
          const deck = this.aiDecks.get(seat);
          if (!deck) {
            throw new AdapterError("P2P_ERROR", `AI seat ${seat} is missing a resolved deck`, false);
          }
          orderedOpponents.push(deck);
          orderedDifficulties.push(kind.data.difficulty);
        }
      }
      if (orderedOpponents.length === 0) {
        throw new AdapterError("P2P_ERROR", "Cannot start P2P game with zero opponents", false);
      }

      if (this.nativeBridge) {
        this.gameStarted = true;
        this.nativeInitialSetupPending = true;
        this.pregameSeatState.gameStarted = true;
        await this.refreshPregameSeatView();

        const allNames = this.playerNamesForSeats();
        this.emit({ type: "playerIdentity", playerId: 0, playerNames: allNames });
        if (this.broker && this.brokerGameCode) {
          void this.broker.unregister(this.brokerGameCode).catch(() => {
            /* best-effort */
          });
        }
        const result = await this.nativeBridge.start();
        this.saveSession();
        return result;
      }

      const deckPayload: DeckListPayload = {
        player: hostDeck.player,
        opponent: orderedOpponents[0],
        ai_decks: orderedOpponents.slice(1),
        ai_difficulties: orderedDifficulties,
        // CR 903.13f(3): this payload is REBUILT field-by-field, so a field
        // present on the constructor argument but not named here is silently
        // discarded before it can reach the engine. Naming it is what carries
        // the Commander Masters partner grant into the game.
        draft_set_codes: hostDeck.draft_set_codes,
        booster_pack_pool: hostDeck.booster_pack_pool,
      };
      const playerCount = allowPartialStart
        ? orderedOpponents.length + 1
        : this.pregameSeatState.seats.length;
      if (this.disposed) await this.bailDisposed(false, "start");
      // The occupancy test, the install, and the multiplayer claim are one
      // engine call. `initializeMultiplayerHostGame` refuses an engine that
      // already holds a game and claims the flag on the line after it installs
      // the state — on a memory-constrained device this is the same worker
      // local play uses, so a client-side probe followed by a separate install
      // would leave a window for a local `initializeGame` to land in between
      // and destroy the hosted game (or be destroyed by it). A refusal arrives
      // as `AdapterErrorCode.ENGINE_OCCUPIED`.
      let transition: BrowserTransition;
      const owner = createHostSessionOwner();
      this.wasmHostOwner = owner;
      try {
        transition = await this.applyBrowserMutation(() => this.wasm.initializeMultiplayerHostGame(
          deckPayload,
          this.formatConfig,
          playerCount,
          this.matchConfig,
          undefined,
          owner,
        ));
      } catch (err) {
        // Nothing to compensate. The engine claims itself only on a successful
        // install, so a rejection — an occupied-engine refusal, a deck error,
        // or "Card database not loaded" when `ensureCardDb` swallowed a fetch
        // failure — leaves the engine byte-for-byte untouched. `claimed: false`
        // is what says so, and it is load-bearing in both directions: `true`
        // would run `resetGameState()` on the shared engine and destroy the
        // live local game a refusal had just protected, while dropping the call
        // entirely would lose the private-adapter worker disposal and the typed
        // "disposed during start" error. gameStore catches the rethrow and
        // shows a toast, so the original error is preserved.
        if (this.disposed) await this.bailDisposed(false, "start");
        await this.wasm.releaseHostSession(false, owner);
        if (this.wasmHostOwner === owner) this.wasmHostOwner = null;
        throw err;
      }
      // The engine now holds this game. Record the claim only now: claiming
      // before the engine accepted would let a refused call's teardown clear
      // state this session never owned.
      if (this.disposed) await this.bailDisposed(true, "start", owner);
      // Checked before the stamp: a host whose lease was superseded mid-start
      // must not take the claim from the host that superseded it. It did
      // install engine state, so it hands that state back rather than leaving
      // it for someone else's teardown to find unclaimed.
      if (!this.ownsAuthority()) {
        await this.wasm.releaseHostSession(true, owner);
        if (this.wasmHostOwner === owner) this.wasmHostOwner = null;
        throw new AdapterError("P2P_ERROR", "Host session superseded", true);
      }
      sharedEngineHost = this.engineClaim;
      this.gameStarted = true;
      this.pregameSeatState.gameStarted = true;
      await this.refreshPregameSeatView();
      this.saveSession();

      const allNames = this.playerNamesForSeats();
      this.emit({ type: "playerIdentity", playerId: 0, playerNames: allNames });

      if (this.broker && this.brokerGameCode) {
        void this.broker.unregister(this.brokerGameCode).catch(() => {
          /* best-effort */
        });
      }

      const revision = ++this.authoritativeRevision;
      let setupFailure: unknown = null;
      await this.enqueueDelivery(async () => {
        for (const [pid, session] of this.guestSessions) {
          const token = this.playerTokens.get(pid)!;
          try {
            const projected = await this.projectTransitionForViewer(
              pid,
              transition.result.events,
              transition.generation,
            );
            const accepted = await this.send(session, {
              type: "game_setup",
              wireProtocolVersion: WIRE_PROTOCOL_VERSION,
              assignedPlayerId: pid,
              playerToken: token,
              revision,
              state: projected.snapshot.state,
              events: projected.events,
              playerNames: allNames,
              ...legalActionsToWire(projected.snapshot),
            });
            if (accepted) this.seedGuestEntry(pid, revision);
          } catch (err) {
            console.error(`[P2PHost] game_setup for seat ${pid} failed:`, err);
            setupFailure ??= err;
          }
        }
      });
      // Isolation buys the LATER seats their frame; it must not also buy
      // silence. Before it, the throw reached the host. A seat that never got
      // `game_setup` waits on a promise that never settles (the guest's
      // `initializeGame` resolves only on `game_setup`/`reconnect_ack`), so the
      // host is the only party that can learn of it — keep the throw.
      if (setupFailure !== null) throw setupFailure;

      await this.runAiLoop();
      return transition.result;
  }

  /**
   * The host seat's own contest click. A native host's contest runs on its server, so
   * the click goes out the seat's authenticated loopback socket exactly as a server
   * client's does (and the adapter reports it took the click). Otherwise this engine
   * hosts the contest itself and the caller stamps the click for `driveContest`.
   */
  submitContestClick(contest: number, seq: number): boolean {
    if (!this.nativeBridge) return false;
    if (isContestSequence(seq) && Number.isSafeInteger(contest)) {
      this.nativeBridge.submitContestClick(0, contest, seq);
    }
    return true;
  }

  /**
   * Host a running Eye to Eye / Mouth to Mouth contest. Guests' clicks arrive as
   * `contest_click` frames and wait here until the next tick; the AI seats are the
   * engine's own `Ai` seats (the caller's `aiPlayers` guess cannot know which
   * non-host seats are human guests). A step that changes the contest is broadcast to
   * the guests like any other transition, and a settle hands the AI its turn back.
   */
  async driveContest(
    nowMs: number,
    _aiPlayers: PlayerId[],
    clicks: ContestClickInput[],
  ): Promise<ContestDriveResult> {
    this.assertNotDisposed();
    const idle: ContestDriveResult = {
      step: "NotInContest",
      events: [],
      log_entries: [],
      contest: null,
    };
    // A native host's contest is driven by its server; its clicks travel through
    // `submitContestClick`. A seat dropping pauses the game, NOT the contest: the
    // engine owns that clock and must keep ticking so it can pause for the absentee
    // and forfeit them once the reconnect grace lapses.
    const contestRunnable =
      this.gameRunState === "running" || this.gameRunState === "paused-disconnect";
    if (!this.ownsAuthority() || this.nativeBridge || !contestRunnable) {
      return idle;
    }
    const aiPlayers = this.aiSeats();
    // A settled contest whose resolution never reached the host screen is delivered
    // (and its AI follow-up run) before the engine is driven any further: the
    // engine reports NotInContest from here on and cannot recreate its events.
    if (!(await this.settleOwedHostPublication())) {
      throw new Error("Contest drive paused: owed host publication could not settle");
    }
    const awayPlayers = [...this.disconnectedSeats.keys()];
    const guestClicks = this.guestContestClicks.drain();
    const ordered = [...clicks, ...guestClicks].sort((a, b) => a.at_ms - b.at_ms);
    let result: ContestDriveResult;
    try {
      result = await this.wasm.driveContest(nowMs, aiPlayers, ordered, awayPlayers);
    } catch (error) {
      // The engine may never have seen the guests' clicks; keep them for the next
      // tick (the host seat's own clicks are the caller's to keep). One the engine did
      // accept is rejected there as a stale sequence, so a retry never counts it twice.
      this.guestContestClicks.restore(guestClicks);
      throw error;
    }
    this.guestContestClicks.setContest(result.contest);
    if (result.step !== "Updated" && result.step !== "Settled") return result;
    const transition = this.stampBrowserMutation({
      events: result.events,
      log_entries: result.log_entries,
    });
    // The host screen first, then the guests (see `publishHostSnapshot`), and before any
    // follow-up AI play: the AI's own snapshots unmount the contest, and a transition
    // published after them would never reach the host's store, resolution events included.
    // The adapter owns this commit, so the caller must not repeat it.
    const hostPublished = await this.publishHostSnapshot(transition.result);
    // The AI's own snapshots would unmount the contest before its resolution was
    // shown: when the host's store is still behind, hold the follow-up for the
    // adapter's retry. Recorded in the same turn as the failure, so that retry cannot
    // see the transition delivered without also seeing the follow-up it owes.
    if (result.step === "Settled" && !hostPublished) this.aiFollowUpOwedAfterPublication = true;
    await this.broadcastStateUpdate(transition);
    if (result.step === "Settled" && hostPublished) {
      await this.runAiLoop();
      void this.persistAuthoritativeState();
    }
    // The adapter owns this commit either way: when the host's snapshot read failed,
    // the transition is retained and delivered by the adapter's own retry, so the
    // caller must not commit (and so double-deliver) it.
    return { ...result, published: true };
  }

  async submitAction(action: GameAction, actor: PlayerId): Promise<SubmitResult> {
    // Host's own UI submissions: `actor` is the host's local PlayerId (the
    // caller — gameStore — derived it from `getPlayerId()`). The host is
    // the trust boundary for its own actions; the engine's guard still
    // verifies the actor against `authorized_submitter(state)`.
    this.assertNotDisposed();
    if (!this.ownsAuthority()) {
      throw new AdapterError("P2P_ERROR", "Host session superseded", true);
    }
    if (this.concedingSeats.has(actor)) {
      throw new AdapterError("P2P_PAUSED", "Player departure is in progress", true);
    }
    if (this.gameRunState !== "running") {
      throw new AdapterError(
        "P2P_PAUSED",
        `Cannot submit action while game state is ${this.gameRunState}`,
        true,
      );
    }
    const transition = await this.applyBrowserMutation(() => this.nativeBridge
      ? this.nativeBridge.submitAction(action, actor)
      : this.wasm.submitAction(action, actor));
    if (isZeroCountDebugCreate(action)) return transition.result;
    await this.broadcastStateUpdate(transition);
    await this.runAiLoop();
    void this.persistAuthoritativeState();
    return transition.result;
  }

  async submitInteraction(
    submission: InteractionSubmission,
    actor: PlayerId,
  ): Promise<SubmitResult> {
    this.assertNotDisposed();
    if (!this.ownsAuthority()) {
      throw new AdapterError("P2P_ERROR", "Host session superseded", true);
    }
    if (this.concedingSeats.has(actor)) {
      throw new AdapterError("P2P_PAUSED", "Player departure is in progress", true);
    }
    if (this.gameRunState !== "running") {
      throw new AdapterError(
        "P2P_PAUSED",
        `Cannot submit interaction while game state is ${this.gameRunState}`,
        true,
      );
    }
    const transition = await this.applyBrowserMutation(() => this.nativeBridge
      ? this.nativeBridge.submitInteraction(submission, actor)
      : this.wasm.submitInteraction(submission, actor));
    await this.broadcastStateUpdate(transition);
    await this.runAiLoop();
    void this.persistAuthoritativeState();
    return transition.result;
  }

  async previewManaPayment(action: GameAction, actor: PlayerId): Promise<ObjectId[]> {
    this.assertNotDisposed();
    if (!this.ownsAuthority()) {
      throw new AdapterError("P2P_ERROR", "Host session superseded", true);
    }
    if (this.gameRunState !== "running") {
      throw new AdapterError(
        "P2P_PAUSED",
        `Cannot preview mana payment while game state is ${this.gameRunState}`,
        true,
      );
    }
    if (this.nativeBridge) return this.nativeBridge.previewManaPayment(action, actor);
    return this.wasm.previewManaPayment(action, actor);
  }

  async previewInteraction(
    request: InteractionPreviewRequest,
    actor: PlayerId,
  ): Promise<InteractionPreview> {
    this.assertNotDisposed();
    if (!this.ownsAuthority()) {
      throw new AdapterError("P2P_ERROR", "Host session superseded", true);
    }
    if (this.gameRunState !== "running") {
      throw new AdapterError(
        "P2P_PAUSED",
        `Cannot preview an interaction while game state is ${this.gameRunState}`,
        true,
      );
    }
    if (this.nativeBridge) return this.nativeBridge.previewInteraction(request, actor);
    return this.wasm.previewInteraction(request, actor);
  }

  /** Releases a complete native-server revision only after every local seat
   * socket has supplied its own filtered view. The native server is the
   * authority for this revision; PeerJS carries it through to terminal
   * correlation rather than inventing another clock. */
  private async handleNativeRevision(
    revision: number,
    reports: Map<PlayerId, NativeSeatReport>,
    terminalReason?: string,
  ): Promise<void> {
    if (!this.ownsAuthority()) return;
    const hostReport = reports.get(0);
    if (!hostReport) return;
    if (revision < this.authoritativeRevision) return;
    this.authoritativeRevision = revision;
    const allNames = this.playerNamesForSeats();
    const sends: Array<Promise<boolean>> = [];
    for (const [pid, session] of this.guestSessions) {
      const report = reports.get(pid);
      if (!report || this.disconnectedSeats.has(pid)) continue;
      if (report.kind === "withheld") {
        // A seat client without a snapshot fails a withheld GameStarted instead of reporting it.
        if (this.nativeInitialSetupPending) continue;
        sends.push(this.send(session, { type: "state_withheld", revision }));
        continue;
      }
      const { update } = report;
      if (this.nativeInitialSetupPending) {
        const token = this.playerTokens.get(pid);
        if (!token) continue;
        sends.push(this.send(session, {
          type: "game_setup",
          wireProtocolVersion: WIRE_PROTOCOL_VERSION,
          assignedPlayerId: pid,
          playerToken: token,
          revision,
          state: update.snapshot.state,
          events: update.events,
          playerNames: allNames,
          ...legalActionsToWire(update.snapshot.legalResult),
        }).then((accepted) => {
          if (accepted) this.seedGuestEntry(pid, revision);
          return accepted;
        }));
      } else {
        sends.push(this.send(session, {
          type: "state_update",
          revision,
          state: update.snapshot.state,
          events: update.events,
          logEntries: update.logEntries,
          ...legalActionsToWire(update.snapshot.legalResult),
        }));
      }
    }
    await Promise.all(sends);
    this.nativeInitialSetupPending = false;
    const hostWasWithheld = this.nativeWithheldSeats.has(0);
    for (const [playerId, report] of reports) {
      this.recordNativeSeatOutcome(playerId, revision, report);
    }
    if (hostReport.kind === "withheld") {
      // `commitWithheldHostTerminal` commits a terminal from the host's Full delivery.
      this.emit({
        type: "stateDelivery",
        withheld: { revision: hostReport.notice.revision, reason: i18n.t("multiplayer:p2pStateWithheld") },
      });
      return;
    }
    const hostUpdate = hostReport.update;
    if (hostWasWithheld) this.emit({ type: "stateDelivery", withheld: null });
    this.emit({
      type: "stateChanged",
      snapshot: hostUpdate.snapshot,
      events: hostUpdate.events,
      logEntries: hostUpdate.logEntries,
    });
    await this.commitTerminalIfComplete(hostUpdate.snapshot, revision, terminalReason);
  }

  /** The single writer of what each native seat was last handed: a view, or a withheld notice. */
  private recordNativeSeatOutcome(playerId: PlayerId, revision: number, report: NativeSeatReport): void {
    if (report.kind === "view") {
      this.nativeDeliveredViews.set(playerId, { revision, snapshot: report.update.snapshot });
      this.nativeWithheldSeats.delete(playerId);
    } else {
      // The last real view stays the seat's reconnect handoff.
      this.nativeWithheldSeats.set(playerId, report.notice);
    }
  }

  /** The host's state was withheld at `revision`, so its recipient-bound Full delivery supplies the view the terminal commits from. */
  private async commitWithheldHostTerminal(revision: number): Promise<void> {
    if (!this.ownsAuthority() || this.nativeWithheldSeats.get(0)?.revision !== revision) return;
    const delivery = this.nativeBridge?.hostTerminalDelivery();
    if (!delivery || delivery.terminalRevision !== revision) return;
    if (!delivery.finalView) {
      this.emit({ type: "terminalUnavailable", message: "Full terminal authority is invalid for seat 0" });
      return;
    }
    const snapshot: EngineSnapshot = {
      state: delivery.finalView,
      legalResult: EMPTY_LEGAL_ACTIONS,
      seq: nextSnapshotSeq(),
    };
    this.recordNativeSeatOutcome(0, revision, { kind: "view", update: { snapshot, events: [], revision } });
    this.emit({ type: "stateDelivery", withheld: null });
    this.emit({ type: "stateChanged", snapshot, events: [] });
    await this.commitTerminalIfComplete(snapshot, revision);
  }

  /** The native server publishes the fault after its final state snapshot.
   * Keep the same ordering over PeerJS and make the host terminal only after
   * every active guest received that final filtered state. */
  private async handleNativeAiDriverFault(
    fault: { id: number; revision: number; message: string },
  ): Promise<void> {
    if (!this.ownsAuthority()) return;

    if (this.nativeAiDriverFault !== null) {
      // A resumed adapter is already terminal because its durable fault was
      // rehydrated before the native bridge reconnects. Accept only that exact
      // replay; a duplicate or a different terminal record must not create a
      // second host error or overwrite the persisted cause.
      if (
        this.nativeAiDriverFault.id !== fault.id
        || this.nativeAiDriverFault.revision !== fault.revision
        || this.nativeAiDriverFault.message !== fault.message
        || this.deliveredNativeAiDriverFault !== null
      ) {
        return;
      }
      this.deliveredNativeAiDriverFault = fault;
      this.emit({ type: "error", message: fault.message });
      return;
    }

    if (this.gameRunState === "terminal") return;
    this.nativeAiDriverFault = fault;
    this.deliveredNativeAiDriverFault = fault;
    this.gameRunState = "terminal";
    this.saveSession();
    await Promise.all([...this.guestSessions].map(async ([playerId, session]) => {
      if (this.disconnectedSeats.has(playerId)) return;
      await this.send(session, { type: "ai_driver_fault", ...fault });
    }));
    this.emit({ type: "error", message: fault.message });
  }

  /**
   * Final state first, terminal statement second. The ordered transport plus
   * the state commitment lets a guest reject a plausible-looking terminal
   * result that belongs to another final position or host incarnation.
   *
   * Per recipient this reads a viewer snapshot of its own, so it carries the
   * same rejection source as the state fan-out and is isolated the same way.
   * Unisolated, one rejected read cost the seat its statement, with no way
   * back — this function returns early once `this.terminalResult` is set; it
   * cost that seat its sweep nomination too, whenever the seat had ACKED the
   * state fan-out's frame at this revision, which leaves the lag clause false
   * and missing a durable receipt the only thing that can nominate it; and it cost
   * the host its own `terminalResult` emit, which clears the prompt overlay,
   * drops the resumable save and supplies the closing reason (the board
   * already reads GameOver from the published snapshot). A failed seat never
   * sends a durable receipt, and the sweep heals it.
   */
  private commitTerminalIfComplete(
    snapshot: EngineSnapshot,
    revision: number,
    reason: string = "Game complete",
  ): Promise<boolean> {
    // One commit at a time: a caller arriving while the commit is in flight (a
    // returning seat's retry, a reconciled Concede) waits for its outcome rather
    // than committing a second, differently identified terminal result.
    if (this.terminalCommitInFlight) return this.terminalCommitInFlight;
    const commit = this.commitTerminalOnce(snapshot, revision, reason).finally(() => {
      if (this.terminalCommitInFlight === commit) this.terminalCommitInFlight = null;
    });
    this.terminalCommitInFlight = commit;
    return commit;
  }

  private async commitTerminalOnce(
    snapshot: EngineSnapshot,
    revision: number,
    reason: string,
  ): Promise<boolean> {
    const { waiting_for: waitingFor } = snapshot.state;
    if (waitingFor.type !== "GameOver") return true;
    if (this.terminalResult) {
      await this.tryAcknowledgeNativeTerminal();
      return true;
    }
    this.authoritativeRevision = Math.max(this.authoritativeRevision, revision);
    let hostTerminalState = snapshot.state;
    let finalViews: Map<PlayerId, GameState> | null = null;
    let display: P2PTerminalResult["display"] = {
      winner: waitingFor.data.winner,
      reason,
    };
    if (this.nativeBridge) {
      try {
        finalViews = await this.nativeBridge.terminalFinalViews(revision);
        const authoritativeHost = finalViews.get(0);
        const authoritativeDisplay = this.nativeBridge.terminalDisplay();
        if (
          !authoritativeHost
          || !authoritativeDisplay
          || authoritativeHost.waiting_for.type !== "GameOver"
          || authoritativeHost.waiting_for.data.winner !== authoritativeDisplay.winner
        ) {
          throw new Error("Full terminal host view and display do not agree");
        }
        hostTerminalState = authoritativeHost;
        display = authoritativeDisplay;
      } catch (err) {
        this.emit({
          type: "terminalUnavailable",
          message: err instanceof Error ? err.message : "Full terminal authority is unavailable",
        });
        return false;
      }
    }
    const createResult = async (
      recipient: PlayerId,
      terminalState: GameState,
    ): Promise<P2PTerminalResult> => ({
      key: this.sessionKey,
      lease: this.authority,
      recipient,
      revision,
      terminalId: crypto.randomUUID(),
      finalStateCommitment: await p2pFinalStateCommitment(terminalState),
      display,
    });
    const result = await createResult(0, hostTerminalState);
    if (!(await commitP2PTerminalResult(result))) {
      this.emit({ type: "terminalUnavailable", message: "Failed to retain P2P terminal result" });
      return false;
    }
    this.terminalResult = result;
    this.gameRunState = "terminal";
    // The terminal is durable: a suspended seat that returns is now served its
    // Full final view instead of a reattachment the retired session refuses.
    if (finalViews) this.nativeBridge?.retainSuspendedTerminalViews(finalViews, revision);
    await Promise.all([...this.guestSessions].map(async ([playerId, session]) => {
      try {
        const terminalState = this.nativeBridge
          ? finalViews?.get(playerId)
          : (await this.wasm.getViewerSnapshot(playerId)).state;
        if (!terminalState) throw new Error(`Full terminal view is missing for seat ${playerId}`);
        if (this.nativeWithheldSeats.get(playerId)?.revision === revision) {
          // The seat was handed a notice at this revision: its final view must precede the statement.
          const snapshot: EngineSnapshot = { state: terminalState, legalResult: EMPTY_LEGAL_ACTIONS, seq: nextSnapshotSeq() };
          this.recordNativeSeatOutcome(playerId, revision, { kind: "view", update: { snapshot, events: [], revision } });
          const delivered = await this.send(session, {
            type: "state_update",
            revision,
            state: terminalState,
            events: [],
            ...legalActionsToWire(EMPTY_LEGAL_ACTIONS),
          });
          if (!delivered) return;
        }
        const recipientResult = await this.terminalResultForRecipient(playerId, terminalState, revision);
        await this.sendTerminalResult(playerId, session, recipientResult);
      } catch (err) {
        console.error(`[P2PHost] terminal delivery for seat ${playerId} failed; the sweep resyncs it:`, err);
      }
      // No receipt is recorded here: only the guest can confirm its durable
      // terminal commit, so the sweep retries the same recipient statement.
    }));
    // The host's `terminalResult` emit unmounts the board, and with it anything that
    // could still deliver an accepted transition's events and log entries — so they go
    // out first. While the host's store is still behind, the statement waits for the
    // adapter's own retry rather than overtaking them.
    if (!(await this.publishRetainedHostTransitions())) {
      this.pendingHostTerminalEmit = result;
      return true;
    }
    if (!this.hostPublicationOwed()) this.disarmHostPublicationRetry();
    this.emit({ type: "terminalResult", result });
    await this.tryAcknowledgeNativeTerminal();
    return true;
  }

  /** A Full terminal payload is retained until every eligible P2P guest has
   * durably committed its exact recipient result. A cleanup-only owner is
   * durable before gameplay resume records are removed or Full ACKs clear
   * recipient-private final views. If a guest commits but its receipt is lost,
   * host resume metadata can remain; the private final payload still has its
   * existing bounded TTL. */
  private async tryAcknowledgeNativeTerminal(): Promise<void> {
    const terminal = this.terminalResult;
    const bridge = this.nativeBridge;
    if (
      !this.nativeTerminalAckReady
      || !terminal
      || !bridge
      || this.nativeTerminalAcknowledged
    ) return;
    if (this.nativeTerminalAckPromise) return this.nativeTerminalAckPromise;

    const pending = (async () => {
      for (const playerId of bridge.playerIds()) {
        if (playerId === 0 || this.eliminatedSeats.has(playerId)) continue;
        const token = this.playerTokens.get(playerId);
        if (token && this.kickedTokens.has(token)) continue;
        if (!this.terminalResultReceipts.has(playerId)) return;
      }
      if (this.gameId) {
        this.nativeTerminalPersistenceClearing = true;
        try {
          const authority = await bridge.terminalCleanupAuthority(terminal.revision);
          const obligation: FullTerminalCleanupObligation = {
            gameId: this.gameId,
            p2pSessionKey: terminal.key,
            p2pTerminalId: terminal.terminalId,
            p2pResult: terminal,
            fullKey: authority.fullKey,
            terminalRevision: authority.terminalRevision,
            deliveries: authority.deliveries,
          };
          await saveFullTerminalCleanupStrict(obligation);
          await clearGameStrict(this.gameId);
          this.nativeTerminalPersistenceCleared = true;
          await bridge.acknowledgeTerminalCleanup(obligation);
          await clearFullTerminalCleanupStrict(this.gameId);
        } catch (err) {
          this.emit({
            type: "terminalUnavailable",
            message: err instanceof Error ? err.message : "Native terminal cleanup is still pending",
          });
          return;
        } finally {
          this.nativeTerminalPersistenceClearing = false;
        }
      } else {
        try {
          await bridge.acknowledgeTerminalDeliveries(terminal.revision);
        } catch (err) {
          this.emit({
            type: "terminalUnavailable",
            message: err instanceof Error ? err.message : "Full terminal acknowledgement failed",
          });
          return;
        }
      }
      this.nativeTerminalAcknowledged = true;
    })();
    this.nativeTerminalAckPromise = pending;
    try {
      await pending;
    } finally {
      if (this.nativeTerminalAckPromise === pending) this.nativeTerminalAckPromise = null;
    }
  }

  /**
   * A terminal statement is recipient-bound because its commitment covers the
   * recipient's filtered final state. Reconnects therefore replay the same
   * statement, making delivery and receipt retries idempotent.
   */
  private async terminalResultForRecipient(
    recipient: PlayerId,
    terminalState: GameState,
    revision: number,
  ): Promise<P2PTerminalResult> {
    const terminal = this.terminalResult;
    if (terminal === null) throw new Error("No terminal result to deliver");
    if (terminalState.waiting_for.type !== "GameOver" || terminal.revision !== revision) {
      throw new Error("Reconnect acknowledgement is not the committed final state");
    }
    const result: P2PTerminalResult = {
      key: this.sessionKey,
      lease: this.authority,
      recipient,
      revision,
      terminalId: `${terminal.terminalId}:recipient:${recipient}`,
      finalStateCommitment: await p2pFinalStateCommitment(terminalState),
      display: terminal.display,
    };
    const issued = this.issuedTerminalResults.get(recipient);
    if (issued && !sameP2PTerminalResult(issued, result)) {
      throw new Error("Recipient terminal statement changed after it was issued");
    }
    // Register before the caller sends: a fast receipt can arrive while
    // PeerSession.send is still settling.
    this.issuedTerminalResults.set(recipient, result);
    return result;
  }

  private async sendTerminalResult(
    recipient: PlayerId,
    session: PeerSession,
    result: P2PTerminalResult,
  ): Promise<boolean> {
    this.terminalReceiptSessions.set(session, recipient);
    const accepted = await this.send(session, { type: "terminal_result", result });
    if (!accepted && !this.terminalResultReceipts.has(recipient)) {
      this.terminalReceiptSessions.delete(session);
    }
    return accepted;
  }

  private recordTerminalReceipt(recipient: PlayerId, sourceSession: PeerSession, result: P2PTerminalResult): void {
    const issued = this.issuedTerminalResults.get(recipient);
    if (!issued || !isValidP2PTerminalResult(result) || !sameP2PTerminalResult(issued, result)) return;
    const isNewReceipt = !this.terminalResultReceipts.has(recipient);
    this.terminalResultReceipts.add(recipient);
    const pending = this.pendingTerminalReconnectAcks.get(recipient);
    if (pending?.session === sourceSession) this.finishTerminalReconnectAfterReceipt(recipient);
    if (isNewReceipt) this.requestNativeTerminalAcknowledgement();
  }

  private requestNativeTerminalAcknowledgement(): void {
    const inFlight = this.nativeTerminalAckPromise;
    if (!inFlight) {
      void this.tryAcknowledgeNativeTerminal();
      return;
    }
    // A previous attempt may have observed another seat before its receipt
    // arrived. Re-evaluate after that attempt releases the single-flight gate.
    void inFlight.then(() => {
      if (!this.nativeTerminalAcknowledged) void this.tryAcknowledgeNativeTerminal();
    }, () => {
      if (!this.nativeTerminalAcknowledged) void this.tryAcknowledgeNativeTerminal();
    });
  }

  private finishTerminalReconnectAfterReceipt(recipient: PlayerId): void {
    const pending = this.pendingTerminalReconnectAcks.get(recipient);
    if (!pending) return;
    if (this.pendingTerminalReconnectAcks.get(recipient) !== pending) return;
    const grace = this.disconnectedSeats.get(recipient);
    if (grace?.timer !== null && grace) clearTimeout(grace.timer);
    this.pendingTerminalReconnectAcks.delete(recipient);
    this.pendingReconnectSessions.delete(recipient);
    this.disconnectedSeats.delete(recipient);
  }

  /**
   * Fan out a state update to every connected guest. Each guest gets its own
   * `ViewerTransitionSnapshot` via the engine's combined filter+legal-actions
   * call (one WASM round-trip per guest instead of two). If the transition was
   * overtaken, the helper deliberately falls back to a current state-only
   * `ViewerSnapshot`. Only the acting guest gets a populated `legalActions` map;
   * non-acting guests receive empty legal actions from the engine-side viewer
   * gate (`legal_actions_for_viewer`).
   * Skips disconnected seats (their state is delivered via `reconnect_ack`).
   *
   * Delivery contract (#7924): only the recipient's own `state_ack` advances
   * its entry in `guestAckedRevisions`. A seat whose viewer read or send
   * fails — or which takes the bytes but never applies them — keeps its old
   * entry and is resynchronized by the redelivery sweep below. One seat's
   * failure aborts neither the other seats nor the terminal close.
   */
  private async broadcastStateUpdate(
    transition: BrowserTransition,
    terminalReason?: string,
  ): Promise<void> {
    return this.enqueueDelivery(() => this.broadcastStateUpdateInner(transition, terminalReason));
  }

  private async broadcastStateUpdateInner(
    transition: BrowserTransition,
    terminalReason?: string,
  ): Promise<void> {
    if (!this.ownsAuthority()) return;
    if (this.nativeBridge) return;
    const revision = ++this.authoritativeRevision;
    const sends: Array<Promise<boolean>> = [];
    for (const [pid, session] of this.guestSessions) {
      if (this.disconnectedSeats.has(pid)) continue;
      try {
        const projected = await this.projectTransitionForViewer(
          pid,
          transition.result.events,
          transition.generation,
        );
        sends.push(this.send(session, {
          type: "state_update",
          revision,
          state: projected.snapshot.state,
          events: projected.events,
          logEntries: projected.fresh ? transition.result.log_entries : undefined,
          ...legalActionsToWire(projected.snapshot),
        }));
      } catch (err) {
        console.error(`[P2PHost] viewer snapshot for seat ${pid} failed; the redelivery sweep resyncs it:`, err);
      }
    }
    await Promise.all(sends);
    const takeback = transition.result.takeback;
    // This read decides whether the game closes AT ALL, and the guest-path
    // `try` around this call would swallow a rejection: `terminalResult` would
    // stay null with nothing to retry it, because a finished game produces no
    // further action to drive another close. Surface it rather than leaving the
    // match silently unclosed.
    let closingSnapshot;
    try {
      closingSnapshot = await this.wasm.getSnapshot();
    } catch (err) {
      console.error("[P2PHost] terminal close could not read the final state:", err);
      this.emit({
        type: "terminalUnavailable",
        message: "Failed to read the authoritative state; a game that just ended stays open until something drives another close",
      });
      // Only the seat restore needs the snapshot; the announce does not.
      if (takeback?.resolution === "approved") this.announceTakeback(resolvedTakebackEvent(takeback));
      return;
    }
    if (takeback?.resolution === "approved") {
      this.restoreRewoundSeats(closingSnapshot);
      this.announceTakeback(resolvedTakebackEvent(takeback));
    }
    await this.commitTerminalIfComplete(closingSnapshot, revision, terminalReason);
  }

  /**
   * Create-only seed: a seat that has been HANDED its handshake gets an entry,
   * at the revision that handshake carried. Never advances an existing entry —
   * `recordGuestAck` is the sole authority for that.
   *
   * Transmission is the right signal for the CREATE and acceptance for the
   * ADVANCE, because the two failure directions are not symmetric: a false
   * positive here (send resolved true, frame parked) leaves the seat entried at
   * a stale revision, so the next revision makes `acked < authoritativeRevision`
   * true and the sweep heals it. A false negative — no entry at all — disarms
   * the sweep permanently, which is the deadlock this whole change exists to
   * close: the guest's own ack is fire-and-forget (`P2PGuestAdapter.send`
   * discards `trySend`'s boolean), so a lost ack would otherwise leave the host
   * with nothing to nominate and the guest with no frame to ack against.
   *
   * Called only for `game_setup` and `reconnect_ack`, never `state_update`: an
   * accepted state frame says nothing about whether the seat can authenticate
   * it, and recording those was the shipped bug.
   */
  private seedGuestEntry(pid: PlayerId, revision: number): void {
    if (!this.guestAckedRevisions.has(pid)) {
      this.guestAckedRevisions.set(pid, revision);
    }
  }

  /**
   * Single authority for ADVANCING `guestAckedRevisions`. Monotonic, so a
   * stale resync ack can never roll a seat back. It NEVER creates: an ack for
   * a seat with no entry is dropped, because creating from one would hand the
   * create-guard's invariant to a remote peer. `handleGuestMessage` is bound
   * at lobby-join, long before any handshake, and the pre-switch authority
   * check only inspects a stamp that is present — so an unsolicited
   * `state_ack` would otherwise create an entry for a seat that never took a
   * `game_setup`, and one carrying a negative revision would make that seat
   * read as permanently lagging, costing a viewer-snapshot round trip and a
   * discarded frame every tick for the life of the match.
   *
   * Dropping it costs nothing on the honest path: the seed is a microtask
   * chained to the same `send` promise, while an ack must cross encode →
   * channel → guest decode → guest handler → guest encode → host decode. The
   * seed always lands first.
   *
   * `revision` arrives from a remote peer, so it is validated HERE, at the
   * trust boundary: `validateMessage` checks only type membership, and no
   * field on any frame is validated anywhere on the decode path. An
   * unvalidated `1e9` would make the lag clause false forever and strand the
   * seat permanently — this bug's own symptom from a single frame.
   *
   * `Number.isInteger` covers what the clamp cannot. A `"99"` is not a case:
   * `Math.min` applies ToNumber and returns a Number, so nothing is ever
   * stored as a string. Nor is `NaN` (from `"abc"` / `undefined`) once this
   * never creates — `NaN > prior` is false, so it cannot be stored. What the
   * guard is actually for is a FLOAT: `2.5` clamps and stores cleanly, and
   * `2.5 < authoritativeRevision` then stays true for every integral revision
   * the host will ever reach, so the seat is nominated every tick forever,
   * each one costing a viewer-snapshot round trip and a duplicate frame.
   *
   * The clamp is safe because `authoritativeRevision` only ever rises within
   * an incarnation, and `Math.min` can only LOWER a recorded ack — costing at
   * most an extra redelivery, never a suppressed one.
   */
  private recordGuestAck(pid: PlayerId, revision: number): void {
    if (!Number.isInteger(revision)) return;
    const capped = Math.min(revision, this.authoritativeRevision);
    const prior = this.guestAckedRevisions.get(pid);
    if (prior === undefined) return;
    if (capped > prior) {
      this.guestAckedRevisions.set(pid, capped);
    }
  }

  private scheduleTerminalReconnectCompletion(pid: PlayerId, session: PeerSession): void {
    const pending = this.pendingTerminalReconnectAcks.get(pid);
    if (
      !pending
      || pending.session !== session
      || !pending.ready
      || !pending.stateAckReceived
      || pending.finishing
    ) return;
    pending.finishing = true;
    void this.enqueueDelivery(() => this.completeTerminalReconnectAfterAck(pid, session));
  }

  private async completeTerminalReconnectAfterAck(pid: PlayerId, session: PeerSession): Promise<void> {
    const pending = this.pendingTerminalReconnectAcks.get(pid);
    if (
      !pending
      || pending.session !== session
      || this.pendingReconnectSessions.get(pid) !== session
      || !this.ownsAuthority()
    ) return;
    try {
      const terminal = this.terminalResult;
      const delivered = this.nativeDeliveredViews.get(pid);
      if (
        !terminal
        || pending.snapshot.state.waiting_for.type !== "GameOver"
        || terminal.revision !== pending.revision
        || (this.nativeBridge && delivered?.revision !== pending.revision)
      ) {
        this.failPendingReconnect(pid, session, "Reconnect terminal state is no longer authoritative");
        return;
      }
      this.recordGuestAck(pid, pending.revision);
      const result = await this.terminalResultForRecipient(pid, pending.snapshot.state, pending.revision);
      if (
        this.pendingTerminalReconnectAcks.get(pid) !== pending
        || this.pendingReconnectSessions.get(pid) !== session
        || !this.ownsAuthority()
      ) return;
      if (!(await this.sendTerminalResult(pid, session, result))) {
        pending.finishing = false;
        return;
      }
      const grace = this.disconnectedSeats.get(pid);
      if (grace?.timer !== null && grace) {
        clearTimeout(grace.timer);
        this.disconnectedSeats.set(pid, { disconnectedAt: grace.disconnectedAt, timer: null });
      }
      if (this.terminalResultReceipts.has(pid)) {
        this.finishTerminalReconnectAfterReceipt(pid);
        this.requestNativeTerminalAcknowledgement();
      } else pending.finishing = false;
    } catch (error) {
      pending.finishing = false;
      traceAdapter("Host", "reconnect-ack-failed", {
        pid,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  /**
   * Whether the sweep owes this seat a resync. Both lag checks go through
   * here — the nomination and `redeliverGuestState`'s own re-check — so the
   * two can never disagree.
   *
   * The create-guard gates BOTH clauses. A seat with no entry was never
   * handed a handshake frame its channel accepted — no `game_setup`, no
   * `reconnect_ack` — and belongs to the setup/reconnect path because a
   * `state_update` cannot stand in for the handshake (the guest discards
   * state frames it cannot authenticate). `undefined < N` is already false,
   * but the terminal clause would otherwise adopt exactly such a seat:
   * `terminalResult` stays set for the rest of the incarnation, and that
   * seat's terminal send never ran either.
   */
  private shouldRedeliver(pid: PlayerId): boolean {
    const acked = this.guestAckedRevisions.get(pid);
    if (acked === undefined) return false;
    if (acked < this.authoritativeRevision) return true;
    return this.terminalResult !== null && !this.terminalResultReceipts.has(pid);
  }

  private startRedeliverySweep(): void {
    if (this.redeliveryTimer !== null) return;
    this.redeliveryTimer = setInterval(() => this.queueLaggingRedeliveries(), STATE_REDELIVERY_TICK_MS);
  }

  /** Sweep entry — runs outside the delivery queue, so it only nominates
   * seats; the actual send happens inside `enqueueDelivery`, which re-checks
   * the lag (a broadcast queued ahead may already have healed the seat, and
   * a nominated seat stays single-flight through queueing and transmission). */
  private queueLaggingRedeliveries(): void {
    if (this.disposed || !this.ownsAuthority()) return;
    for (const pid of this.guestSessions.keys()) {
      if (this.disconnectedSeats.has(pid)) continue;
      if (!this.shouldRedeliver(pid) || this.pendingRedeliveries.has(pid)) continue;
      this.pendingRedeliveries.add(pid);
      void this.enqueueDelivery(async () => {
        try {
          await this.redeliverGuestState(pid);
        } finally {
          this.pendingRedeliveries.delete(pid);
        }
      });
    }
    // A terminal reconnect channel is not in `guestSessions` until its handoff
    // ACK arrives, so the loop above never reaches it. A lost or undecodable
    // ACK would otherwise strand the seat: the host never sends its terminal
    // result, and a fresh reconnect is refused while this one is pending.
    for (const [pid, pending] of this.pendingTerminalReconnectAcks) {
      if (!pending.ready || this.terminalResultReceipts.has(pid) || this.pendingRedeliveries.has(pid)) continue;
      this.pendingRedeliveries.add(pid);
      void this.enqueueDelivery(async () => {
        try {
          if (pending.stateAckReceived) this.scheduleTerminalReconnectCompletion(pid, pending.session);
          else await this.redeliverPendingTerminalHandoff(pid, pending.session);
        } finally {
          this.pendingRedeliveries.delete(pid);
        }
      });
    }
  }

  /**
   * Re-sends a terminal reconnect's handoff state as a `state_update` at the
   * handoff revision. The guest installed this channel on `reconnect_ack`, and
   * its strict `<` stale guard lets an equal-revision frame through to a fresh
   * `state_ack`, which `handlePendingTerminalReconnectFrame` then consumes. A
   * refused send leaves the seat for the next sweep.
   */
  private async redeliverPendingTerminalHandoff(pid: PlayerId, session: PeerSession): Promise<void> {
    if (this.disposed || !this.ownsAuthority()) return;
    const pending = this.pendingTerminalReconnectAcks.get(pid);
    if (!pending || pending.session !== session || !pending.ready || pending.stateAckReceived) return;
    await this.send(session, {
      type: "state_update",
      revision: pending.revision,
      state: pending.snapshot.state,
      events: [],
      ...legalActionsToWire(pending.snapshot.legalResult),
    });
  }

  /**
   * Idempotent per-seat resync: reuses `reconnectHandoff` (revision and
   * viewer snapshot captured together, native and WASM alike) and sends the
   * CURRENT authoritative state as a plain `state_update`. The missed frame's
   * events and log entries are not replayed — the full state is the recovery;
   * animations and the seat's game log are not, and the log keeps that hole.
   * If the game has already closed, the seat also gets the same recipient
   * `terminal_result` AFTER the healing state frame: the one-shot terminal
   * fan-out ran against the seat's stale revision, and `acceptTerminalResult`
   * refuses a revision mismatch, so without this the seat would hold the
   * final board but never a committed result. Never rejects: a failure
   * leaves the seat behind and the next sweep retries, for as long as the
   * seat stays connected.
   */
  private async redeliverGuestState(pid: PlayerId): Promise<void> {
    if (this.disposed || !this.ownsAuthority()) return;
    const session = this.guestSessions.get(pid);
    if (!session || this.disconnectedSeats.has(pid)) return;
    if (!this.shouldRedeliver(pid)) return;
    try {
      const withheld = this.nativeWithheldSeats.get(pid);
      if (withheld) {
        // A lost notice heals into the notice, never into the seat's older board.
        await this.send(session, { type: "state_withheld", revision: withheld.revision });
        return;
      }
      const handoff = await this.reconnectHandoff(pid);
      const accepted = await this.send(session, {
        type: "state_update",
        revision: handoff.revision,
        state: handoff.snapshot.state,
        events: [],
        ...legalActionsToWire(handoff.snapshot.legalResult),
      });
      if (!accepted) return;
      if (this.terminalResult !== null) {
        // Recipient-bound, like the reconnect path: `terminalResultForRecipient`
        // verifies the terminal revision against this handoff and throws
        // otherwise — the catch below turns that into a retry. The state frame
        // is settled by the seat's own ack; only the terminal frame needs a
        // write here, so a refused or failed terminal send re-nominates the
        // seat on the next sweep even after the state ack lands.
        const result = await this.terminalResultForRecipient(pid, handoff.snapshot.state, handoff.revision);
        await this.sendTerminalResult(pid, session, result);
      }
    } catch (err) {
      console.warn(`[P2PHost] state redelivery for seat ${pid} failed; retrying on the next sweep:`, err);
    }
  }

  /**
   * Emit the host's own `stateChanged` for an applied submission.
   *
   * Call this BEFORE `broadcastStateUpdate`, never after (#7924). On the
   * guest-message paths the fan-out runs inside a `try`, and it can still
   * reject at its close: the host snapshot read feeding
   * `commitTerminalIfComplete`. The per-seat viewer reads on both sides of
   * that close — the fan-out's and the terminal commit's own recipient-bound
   * ones — are caught per seat and left to the redelivery sweep. Emitting
   * afterwards therefore makes the host's own screen depend on work done on
   * every guest's behalf — the host freezes on a board its own engine has
   * already advanced.
   *
   * Precise about the failure that is NOT in that set: a dead guest channel
   * is not one. `trySend` resolves `false` for a closed channel, an encode
   * error, or a throwing `conn.send` (`network/peer.ts:69-106`), so a broken
   * link degrades the fan-out silently rather than rejecting it. The ordering
   * matters for the per-viewer snapshot and the terminal commit.
   *
   * **This never rejects.** Running before the fan-out would otherwise turn a
   * failed local read into a stalled delivery: the fan-out is the only step
   * that advances the authoritative revision, so skipping it leaves even the
   * redelivery sweep with nothing to nominate — the guests hear nothing until
   * the next action — and their watchdog cannot bridge that gap
   * (`game/staleStateWatchdog.ts` — its check
   * compares screen against the adapter cache, and an undelivered update
   * leaves both equally stale). Symmetry is the whole point of the ordering:
   * on THIS path neither side may starve the other, so a failure here is
   * logged and the caller continues to the fan-out.
   *
   * The symmetry stops at the guest-message paths. The host's own
   * `submitAction` / `submitInteraction` still await the fan-out without a
   * `try`, so its one remaining rejection — the terminal close — surfaces to
   * the host's caller after the per-seat sends have settled; a seat that
   * missed its frame belongs to the ledger and sweep. Naming it rather than
   * changing a public adapter contract in this PR.
   *
   * A failed publication is not a dropped one: the engine has already accepted
   * the transition, so nothing could recreate its events. It is retained and goes
   * out, in order and exactly once, with the next successful publication — which the
   * adapter's own retry (`armHostPublicationRetry`) guarantees even when nothing else
   * happens, and which a terminal close waits for before the host's board unmounts.
   * Resolves `true` once the host's store has been told, `false` while it is still
   * behind.
   *
   * No-op under `nativeBridge`, which publishes its own revisions.
   */
  private publishHostSnapshot(result: SubmitResult): Promise<boolean> {
    if (this.nativeBridge) return Promise.resolve(true);
    const published = this.hostPublicationQueue.then(() => this.publishHostSnapshotInner(result));
    this.hostPublicationQueue = published.then(() => undefined);
    return published;
  }

  private async publishHostSnapshotInner(result: SubmitResult): Promise<boolean> {
    // Anything an earlier failed publication left behind goes out first and in the
    // same frame, so the host's store sees every accepted transition's events once.
    const transitions = [...this.unpublishedHostTransitions, result];
    try {
      this.emit({
        type: "stateChanged",
        snapshot: await this.wasm.getSnapshot(),
        events: transitions.flatMap((t) => t.events),
        logEntries: transitions.some((t) => t.log_entries)
          ? transitions.flatMap((t) => t.log_entries ?? [])
          : undefined,
      });
      this.unpublishedHostTransitions = [];
      return true;
    } catch (err) {
      console.error("[P2PHost] host snapshot publication failed:", err);
      this.unpublishedHostTransitions = transitions;
      // Whoever published may be the last thing to touch the host this session (an
      // AI or guest action that enters a contest, or the action that ends the game),
      // so the retry cannot be left to a later tick of some mounted component.
      this.armHostPublicationRetry();
      return false;
    }
  }

  /** Whether the host's store or the AI is still owed something from a failed publication. */
  private hostPublicationOwed(): boolean {
    return this.unpublishedHostTransitions.length > 0
      || this.aiFollowUpOwedAfterPublication
      || this.pendingHostTerminalEmit !== null;
  }

  private armHostPublicationRetry(): void {
    if (this.hostPublicationTimer !== null || this.disposed) return;
    this.hostPublicationTimer = setInterval(
      () => void this.retryOwedHostPublication(),
      HOST_PUBLICATION_RETRY_TICK_MS,
    );
  }

  private disarmHostPublicationRetry(): void {
    if (this.hostPublicationTimer === null) return;
    clearInterval(this.hostPublicationTimer);
    this.hostPublicationTimer = null;
  }

  private async retryOwedHostPublication(): Promise<void> {
    if (this.disposed || !this.ownsAuthority()) {
      // A superseded or torn-down host has no screen left to catch up.
      this.unpublishedHostTransitions = [];
      this.aiFollowUpOwedAfterPublication = false;
      this.pendingHostTerminalEmit = null;
      this.disarmHostPublicationRetry();
      return;
    }
    try {
      await this.settleOwedHostPublication();
    } catch (err) {
      console.error("[P2PHost] retrying an owed host publication failed:", err);
    }
    if (!this.hostPublicationOwed()) this.disarmHostPublicationRetry();
  }

  /**
   * Settle what a failed host publication left owed, in the order it was owed: the
   * retained transitions, then the terminal statement or the AI follow-up they were
   * holding back. `false` while the host screen is still behind the engine. Safe to
   * call from anywhere — concurrent callers share one settlement.
   */
  private settleOwedHostPublication(): Promise<boolean> {
    this.hostSettlement ??= this.settleOwedHostPublicationInner().finally(() => {
      this.hostSettlement = null;
    });
    return this.hostSettlement;
  }

  private async settleOwedHostPublicationInner(): Promise<boolean> {
    if (!(await this.publishRetainedHostTransitions())) return false;
    const terminal = this.pendingHostTerminalEmit;
    if (terminal !== null) {
      this.pendingHostTerminalEmit = null;
      this.emit({ type: "terminalResult", result: terminal });
    }
    // A finished game has no AI turn left to give.
    if (this.terminalResult !== null) this.aiFollowUpOwedAfterPublication = false;
    // A paused game keeps its follow-up: the engine must not be driven from here, and
    // nothing else knows the AI is owed a turn.
    if (this.aiFollowUpOwedAfterPublication && this.gameRunState === "running") {
      this.aiFollowUpOwedAfterPublication = false;
      await this.runAiLoop();
      void this.persistAuthoritativeState();
    }
    if (!this.hostPublicationOwed()) this.disarmHostPublicationRetry();
    return true;
  }

  /** Deliver the retained transitions, if any, on their own. */
  private async publishRetainedHostTransitions(): Promise<boolean> {
    if (this.unpublishedHostTransitions.length === 0) return true;
    return this.publishHostSnapshot({ events: [] });
  }

  async getState(): Promise<GameState> {
    this.assertNotDisposed();
    if (this.nativeBridge) return this.nativeBridge.getState();
    return this.wasm.getState();
  }

  async getLegalActions(): Promise<LegalActionsResult> {
    this.assertNotDisposed();
    if (this.nativeBridge) return this.nativeBridge.getLegalActions();
    return this.wasm.getLegalActions();
  }

  /** The host owns the engine — delegate to the inner WASM adapter, which
   *  stamps the seq when the worker response arrives. (The broadcast path
   *  `getViewerSnapshot` deliberately does NOT consume the counter: guests
   *  stamp arrival order on their own ordered channel, and `seq` is never
   *  compared across clients.) */
  async getSnapshot(): Promise<EngineSnapshot> {
    this.assertNotDisposed();
    if (this.nativeBridge) return this.nativeBridge.getSnapshot();
    return this.wasm.getSnapshot();
  }

  /**
   * Returns the already-completed host-resume transition once. The transition
   * itself happens inside `initialize()` before this adapter exposes a state
   * to reconnecting guests; this method only hands its atomic result to the
   * local store.
   */
  async resumeRestoredGameState(): Promise<RestoredGameStateResult | null> {
    const resumed = this.resumedAutomation;
    this.resumedAutomation = null;
    return resumed;
  }

  getAiActionProposal(
    difficulty: string,
    playerId: number,
  ): Promise<AiActionProposal | null> | AiActionProposal | null {
    // Rejected rather than thrown: callers wrap this in `Promise.resolve(...)`
    // without a synchronous try, matching what a disposed private engine did.
    if (this.disposed) return Promise.reject(hostDisposedError());
    return this.nativeBridge
      ? null
      : this.wasm.getAiActionProposal(difficulty, playerId);
  }


  async submitAiActionProposal(
    proposal: AiActionProposal,
  ): Promise<AiProposalSubmission> {
    this.assertNotDisposed();
    if (!this.ownsAuthority()) {
      return { status: "stale", reason: "P2P host authority changed" };
    }
    if (this.gameRunState !== "running") {
      throw new AdapterError(
        "P2P_PAUSED",
        `Cannot submit AI proposal while game state is ${this.gameRunState}`,
        true,
      );
    }
    if (this.nativeBridge) {
      return { status: "stale", reason: "native P2P authority owns AI decisions" };
    }
    const outcome = await this.wasm.submitAiActionProposal(proposal);
    if (outcome.status === "applied") {
      const transition = this.stampBrowserMutation(outcome.result);
      await this.broadcastStateUpdate(transition);
      await this.runAiLoop();
      void this.persistAuthoritativeState();
    }
    return outcome;
  }

  restoreState(_state: PersistedGameState): void {
    throw new AdapterError("P2P_ERROR", "Undo not supported in P2P games", false);
  }

  estimateBracket(_deck: BracketDeckRequest): Promise<BracketEstimate | null> {
    throw new AdapterError(
      AdapterErrorCode.BRACKET_ESTIMATION_UNSUPPORTED,
      "Bracket estimation is a local feature; not available in P2P sessions.",
      false,
    );
  }

  async sendConcede(): Promise<void> {
    if (!this.ownsAuthority()) return;
    const outcome = await this.concedePlayer(0, "Host conceded", "conceded");
    if (outcome !== "committed") return;
    for (const [, s] of this.guestSessions) {
      void this.send(s, { type: "player_conceded", playerId: 0, reason: "Host conceded" });
    }
  }

  /**
   * A draft match installs this only with its pod-issued capability. The
   * adapter deliberately does not synthesize a result from a room code.
   */
  sendMatchConcede(): void {
    this.requestBoundMatchConcede(0);
  }

  sendRequestTakeback(): void {
    if (!this.ownsAuthority()) return;
    void this.requestTakeback(0);
  }

  sendRespondTakeback(approve: boolean): void {
    if (!this.ownsAuthority()) return;
    void this.respondTakeback(0, approve);
  }

  sendCancelTakeback(): void {
    if (!this.ownsAuthority()) return;
    void this.cancelTakeback(0);
  }

  private aiSeats(): PlayerId[] {
    return this.pregameSeatState.seats.flatMap((seat, index) => (seat.type === "Ai" ? [index] : []));
  }

  private async requestTakeback(pid: PlayerId): Promise<void> {
    if (this.nativeBridge) {
      this.nativeBridge.requestTakeback(pid);
      return;
    }
    await this.answerTakeback(pid, () => this.wasm.requestTakeback(pid, this.aiSeats()));
  }

  private async respondTakeback(pid: PlayerId, approve: boolean): Promise<void> {
    if (this.nativeBridge) {
      this.nativeBridge.respondTakeback(pid, approve);
      return;
    }
    await this.answerTakeback(pid, () => this.wasm.respondTakeback(pid, approve, this.aiSeats()));
  }

  private async cancelTakeback(pid: PlayerId): Promise<void> {
    if (this.nativeBridge) {
      this.nativeBridge.cancelTakeback(pid);
      return;
    }
    await this.answerTakeback(pid, () => this.wasm.cancelTakeback(pid));
  }

  /** Run one browser takeback call for `pid` and publish what the engine replied. */
  private async answerTakeback(pid: PlayerId, call: () => Promise<TakebackReply>): Promise<void> {
    let reply: TakebackReply;
    try {
      reply = await call();
    } catch (err) {
      console.error(`[P2PHost] takeback call for seat ${pid} failed:`, err);
      const reason = err instanceof Error ? err.message : String(err);
      await this.enqueueDelivery(async () => this.deliverTakebackEvent(pid, { type: "requestRejected", reason }));
      return;
    }
    try {
      await this.publishTakebackReply(pid, reply);
    } catch (err) {
      console.error("[P2PHost] delivery after a takeback reply failed:", err);
    }
  }

  /** The server transport's arms for the same replies, one for one. */
  private async publishTakebackReply(pid: PlayerId, reply: TakebackReply): Promise<void> {
    switch (reply.status) {
      case "requested": {
        const event: TakebackEvent = {
          type: "takebackRequested",
          requester: reply.requester,
          requesterName: this.playerNamesForSeats()[reply.requester] ?? "",
          voters: reply.voters,
        };
        await this.enqueueDelivery(async () => this.announceTakeback(event));
        return;
      }
      case "recorded":
        return;
      case "rejected": {
        const event: TakebackEvent = { type: "requestRejected", reason: reply.reason };
        await this.enqueueDelivery(async () => this.deliverTakebackEvent(pid, event));
        return;
      }
      case "resolved": {
        const notice: TakebackNotice = { resolution: reply.resolution, resolvedBy: reply.resolvedBy };
        if (notice.resolution === "approved") {
          const transition = this.stampBrowserMutation({ events: [], log_entries: [], takeback: notice }, true);
          await this.publishHostSnapshot(transition.result);
          await this.broadcastStateUpdate(transition);
          void this.persistAuthoritativeState();
        } else {
          await this.enqueueDelivery(async () => this.announceTakeback(resolvedTakebackEvent(notice)));
        }
        // The AI loop stops while a vote is open; every resolution restarts it.
        await this.runAiLoop();
        return;
      }
    }
  }

  /** Called only inside `enqueueDelivery`, so a notice cannot land between a reconnect's replay and its promotion. */
  private deliverTakebackEvent(pid: PlayerId, event: TakebackEvent): void {
    if (pid === 0) {
      this.emit(event);
      return;
    }
    const session = this.guestSessions.get(pid);
    if (!session || this.disconnectedSeats.has(pid)) return;
    void this.send(session, takebackEventToWire(event));
  }

  private announceTakeback(event: TakebackEvent): void {
    this.deliverTakebackEvent(0, event);
    for (const pid of this.guestSessions.keys()) this.deliverTakebackEvent(pid, event);
  }

  /** A native seat's takeback notice, relayed to that seat as the server sent it. */
  private async handleNativeSeatNotice(pid: PlayerId, event: TakebackEvent): Promise<void> {
    if (!this.ownsAuthority() || !this.nativeBridge) return;
    // Seat 0 is the table's single trigger: the server sends every vote notice to every seat.
    if (pid === 0 && event.type === "takebackResolved" && event.resolution === "approved") {
      this.restoreRewoundSeats(await this.nativeBridge.getSnapshot());
    }
    this.deliverTakebackEvent(pid, event);
  }

  /**
   * The sole match-concession sink. Both the local host control and a guest's
   * protected wire request pass through this authority-bound route.
   */
  private requestBoundMatchConcede(concedingPlayer: PlayerId): void {
    if (!this.boundMatchConcede || this.matchConcedeSent || !this.ownsAuthority()) return;
    if (!this.gameStarted || this.gameRunState !== "running") return;
    this.matchConcedeSent = true;
    void Promise.resolve(this.boundMatchConcede.onConcede(concedingPlayer)).catch(() => {
      this.matchConcedeSent = false;
    });
  }

  /**
   * Release all transport + engine resources. PRESERVES the persisted
   * resume record so a subsequent reload can pick up the game. Called
   * on React unmount (navigation, StrictMode remount, tab close).
   *
   * Explicit user quit goes through `terminateGame()` instead, which
   * clears the persistence before disposing.
   */
  dispose(): void {
    // Set first and synchronously: every in-flight init/start re-checks this
    // after each await, and callers that kept a reference must fail loud.
    this.disposed = true;
    this.unsubscribeHostConnections();
    for (const { timer } of this.disconnectedSeats.values()) {
      if (timer !== null) clearTimeout(timer);
    }
    this.disconnectedSeats.clear();
    this.concedingSeats.clear();
    for (const session of this.pendingReconnectSessions.values()) {
      session.close();
    }
    this.pendingReconnectSessions.clear();
    this.pendingTerminalReconnectAcks.clear();
    for (const session of this.guestSessions.values()) {
      session.close();
    }
    this.guestSessions.clear();
    this.kickedTokens.clear();
    this.playerTokens.clear();
    this.guestDecks.clear();
    this.aiDecks.clear();
    this.nativeDeliveredViews.clear();
    this.nativeWithheldSeats.clear();
    this.guestAckedRevisions.clear();
    this.pendingRedeliveries.clear();
    this.issuedTerminalResults.clear();
    this.terminalResultReceipts.clear();
    this.terminalReceiptSessions = new WeakMap();
    if (this.redeliveryTimer !== null) {
      clearInterval(this.redeliveryTimer);
      this.redeliveryTimer = null;
    }
    this.disarmHostPublicationRetry();
    this.unpublishedHostTransitions = [];
    this.aiFollowUpOwedAfterPublication = false;
    this.pendingHostTerminalEmit = null;
    try {
      this.hostPeer.destroy();
    } catch {
      /* best-effort */
    }
    // Native authority is persisted by phase-server. A component unmount is
    // intentionally not an abandonment: a remount/reload reconnects with the
    // stored local tokens. Explicit termination below sends AbandonGame.
    this.nativeBridge?.dispose();
    // Release only what this session owns. A private engine is disposed as
    // before; a shared one keeps its worker and card DB and has its state
    // cleared only when this adapter is the recorded claimant — so a stale or
    // never-started host cannot wipe a live claimant's game (or a local game
    // that was never a host's to begin with). Idempotent: `dispose()` really is
    // called twice on the same instance (GameProvider disposes directly, then
    // `gameStore.reset()` disposes it again), and the second pass takes the
    // unclaimed branch. Fire-and-forget from a synchronous `dispose()`.
    const owner = this.wasmHostOwner;
    this.wasmHostOwner = null;
    if (sharedEngineHost === this.engineClaim) {
      sharedEngineHost = null;
      if (owner) void this.wasm.releaseHostSession(true, owner);
    } else {
      if (owner) void this.wasm.releaseHostSession(false, owner);
      else void this.wasm.releaseHostSession(false);
    }
    releaseP2PHostLease(this.authority);
    // Close the broker only when the adapter owns it. When the multiplayer
    // store owns the broker (externally managed), it survives adapter disposal
    // so the lobby entry stays alive across page navigations.
    if (this.ownsBroker) {
      this.broker?.close();
    }
    this.listeners = [];
  }

  /**
   * Explicit user quit — clears the persisted resume record so the
   * menu's Resume button won't surface this game next session, then
   * delegates to `dispose()` for teardown.
   *
   * Callers: "Leave game" affordance, game-over cleanup, concede flows
   * that should end the session permanently. Should NOT be called from
   * component unmount / tab close / StrictMode remount — those need
   * persistence preserved and go through `dispose()`.
   */
  async terminateGame(): Promise<void> {
    if (!this.ownsAuthority()) {
      this.dispose();
      return;
    }
    // Notify every live guest session BEFORE dispose tears the sessions down.
    // Without this, guests interpret the ensuing DataConnection close as a
    // transient network drop and burn through the full reconnect backoff
    // (minutes of doomed retries against a Peer that was just destroyed).
    // The wire message is sent synchronously while the sessions are still
    // open; PeerJS buffers the RTCDataChannel write, and `dispose()` below
    // runs on the next line so the message flushes before the channel tears
    // down. This broadcast is intentionally skipped on `dispose()` — plain
    // unmounts (StrictMode remount, tab close, navigation) may be transient
    // and the guest's reconnect loop is the correct behavior there.
    // Await `host_left` flushes before disposing — `dispose()` tears down
    // sessions, so any not-yet-flushed bytes would race the close. Adapter
    // contract: `await terminateGame()` returns once every guest has
    // received the farewell (or the channel was already gone).
    await Promise.all(
      [...this.guestSessions.values()].map((s) =>
        this.send(s, { type: "host_left", reason: "Host left the game" }),
      ),
    );
    await this.nativeBridge?.abandon().catch(() => {
      /* best-effort teardown: persisted tombstone is cleared below */
    });
    if (this.gameId) {
      void clearP2PHostSession(this.gameId);
    }
    this.dispose();
  }

  private guestPlayerIdForSession(sourceSession: PeerSession): PlayerId | null {
    for (const [pid, session] of this.guestSessions) {
      if (session === sourceSession) return pid;
    }
    return null;
  }

  private authenticatedGuestPlayerId(sourceSession: PeerSession, msg: P2PMessage): PlayerId | null {
    // Resolve the current seat: mutations can compact the map while a channel stays open.
    const pid = this.guestPlayerIdForSession(sourceSession);
    if (pid === null) return null;
    const session = this.guestSessions.get(pid);
    // Pre-ACK reconnect channels are not installed and cannot advance a seat.
    if (session !== sourceSession) return null;
    if (!this.ownsAuthority()) {
      this.rejectSuperseded(session);
      return null;
    }
    if (msg.authority && !hasExactP2PAuthority(msg.authority, this.authority)) {
      this.rejectSuperseded(session);
      return null;
    }
    return pid;
  }

  /**
   * A terminal reconnect channel is not installed in `guestSessions` until its
   * ACK has been delivered, so it may only deliver the state ACK for its pending
   * terminal revision and the matching durable terminal receipt. Returns true
   * when `sourceSession` is such a channel and the frame has been consumed.
   */
  private handlePendingTerminalReconnectFrame(sourceSession: PeerSession, msg: P2PMessage): boolean {
    if (this.guestPlayerIdForSession(sourceSession) !== null) return false;
    const pendingTerminal = [...this.pendingTerminalReconnectAcks]
      .find(([, pending]) => pending.session === sourceSession);
    if (!pendingTerminal) return false;
    const [pendingPid, pending] = pendingTerminal;
    if (!this.ownsAuthority() || (msg.authority && !hasExactP2PAuthority(msg.authority, this.authority))) {
      this.rejectSuperseded(sourceSession);
      return true;
    }
    if (msg.type === "terminal_result_receipt") {
      if (!msg.authority || !hasExactP2PAuthority(msg.authority, this.authority)) return true;
      this.recordTerminalReceipt(pendingPid, sourceSession, msg.result);
      return true;
    }
    if (msg.type !== "state_ack" || !Number.isInteger(msg.revision) || msg.revision !== pending.revision) return true;
    pending.stateAckReceived = true;
    this.scheduleTerminalReconnectCompletion(pendingPid, sourceSession);
    return true;
  }

  private handleGuestStateAck(sourceSession: PeerSession, msg: Extract<P2PMessage, { type: "state_ack" }>): void {
    if (this.handlePendingTerminalReconnectFrame(sourceSession, msg)) return;
    const pid = this.authenticatedGuestPlayerId(sourceSession, msg);
    if (pid !== null) this.recordGuestAck(pid, msg.revision);
  }

  private async handleGuestMessage(
    sourceSession: PeerSession,
    msg: P2PMessage,
  ): Promise<void> {
    if (this.handlePendingTerminalReconnectFrame(sourceSession, msg)) return;
    if (msg.type === "terminal_result_receipt") {
      const pid = this.terminalReceiptSessions.get(sourceSession) ?? this.guestPlayerIdForSession(sourceSession);
      if (pid === null || pid === undefined) return;
      if (!this.ownsAuthority()) {
        this.rejectSuperseded(sourceSession);
        return;
      }
      if (!msg.authority || !hasExactP2PAuthority(msg.authority, this.authority)) return;
      this.recordTerminalReceipt(pid, sourceSession, msg.result);
      return;
    }
    const pid = this.authenticatedGuestPlayerId(sourceSession, msg);
    if (pid === null) return;
    const session = sourceSession;
    if (this.concedingSeats.has(pid) && (msg.type === "action" || msg.type === "interaction")) {
      void this.send(sourceSession, { type: "action_failed", message: "Player departure is in progress" });
      return;
    }
    switch (msg.type) {
      case "action": {
        // Verify sender identity to prevent guest 2 spoofing as guest 3.
        if (msg.senderPlayerId !== pid) {
          const session = this.guestSessions.get(pid);
          if (session) {
            void this.send(session, {
              type: "action_failed",
              message: `senderPlayerId mismatch (declared ${msg.senderPlayerId}, session owns ${pid})`,
            });
          }
          console.warn(
            `[P2PHost] rejected action from seat ${pid} with declared sender ${msg.senderPlayerId}`,
          );
          return;
        }
        // Short-circuit: an eliminated seat (post-concede) has no legal
        // actions in the engine. Reject at the adapter so the wire log is
        // clear and the WASM round-trip is skipped.
        if (this.eliminatedSeats.has(pid)) {
          const session = this.guestSessions.get(pid);
          if (session) {
            void this.send(session, {
              type: "action_failed",
              message: "Player has conceded and can no longer act",
            });
          }
          return;
        }
        if (this.gameRunState !== "running") {
          const session = this.guestSessions.get(pid);
          if (session) {
            void this.send(session, {
              type: "action_failed",
              message: `Game ${this.gameRunState}`,
            });
          }
          return;
        }
        let transition: BrowserTransition;
        try {
          // CRITICAL: pass `pid` (the session-bound PlayerId), NEVER
          // `msg.senderPlayerId`. The envelope check above already guarantees
          // they match, but if we ever regressed that check we must still
          // tag with the authenticated session identity — the wire payload
          // is untrusted. This is the defense-in-depth that makes the engine
          // guard meaningful for P2P.
          transition = await this.applyBrowserMutation(() => this.nativeBridge
            ? this.nativeBridge.submitAction(msg.action, pid)
            : this.wasm.submitAction(msg.action, pid));
        } catch (err) {
          // The engine refused the action: nothing applied, so this — and only
          // this — is an action failure the guest must hear about.
          const session = this.guestSessions.get(pid);
          if (session) void this.send(session, actionFailureFrame(err));
          break;
        }
        // Past here the action HAS applied. Everything left is delivery and
        // bookkeeping, so a throw must not reach the guest as an action
        // failure — that reports an applied action as failed and the guest's
        // screen then disagrees with the authoritative engine (#7924).
        try {
          if (isZeroCountDebugCreate(msg.action)) {
            const session = this.guestSessions.get(pid);
            if (session) await this.send(session, { type: "action_noop" });
            break;
          }
          // Host screen first, then the guests (see `publishHostSnapshot`).
          await this.publishHostSnapshot(transition.result);
          await this.broadcastStateUpdate(transition);
          // Wake the AI loop. After a guest's action lands, priority may have
          // shifted to an AI seat — without this, the AI never gets a turn
          // and the game stalls (same pattern as concedePlayer/host submit).
          await this.runAiLoop();
          void this.persistAuthoritativeState();
        } catch (err) {
          console.error("[P2PHost] delivery after an applied guest action failed:", err);
        }
        break;
      }
      case "interaction": {
        const session = this.guestSessions.get(pid);
        if (!session || msg.senderPlayerId !== pid) {
          if (session) void this.send(session, { type: "action_failed", message: "senderPlayerId mismatch" });
          return;
        }
        if (this.eliminatedSeats.has(pid) || this.gameRunState !== "running") {
          void this.send(session, {
            type: "action_failed",
            message: this.eliminatedSeats.has(pid)
              ? "Player has conceded and can no longer act"
              : `Game ${this.gameRunState}`,
          });
          return;
        }
        let transition: BrowserTransition;
        try {
          transition = await this.applyBrowserMutation(() => this.nativeBridge
            ? this.nativeBridge.submitInteraction(msg.submission, pid)
            : this.wasm.submitInteraction(msg.submission, pid));
        } catch (err) {
          void this.send(session, actionFailureFrame(err));
          break;
        }
        // Applied — same delivery contract as the "action" case above (#7924).
        try {
          await this.publishHostSnapshot(transition.result);
          await this.broadcastStateUpdate(transition);
          await this.runAiLoop();
          void this.persistAuthoritativeState();
        } catch (err) {
          console.error("[P2PHost] delivery after an applied guest interaction failed:", err);
        }
        break;
      }
      case "contest_click": {
        // Fire-and-forget, never answered. The seat is the authenticated session's
        // (`pid`), the instant is this host's own clock, and the engine's state
        // machine rejects anything stale, duplicate, early or late — so a guest can
        // claim neither a win nor a timestamp. An eliminated or paused seat's click
        // is simply dropped.
        if (this.eliminatedSeats.has(pid)) return;
        if (this.nativeBridge) {
          // The native server owns the contest: relay on the seat's own authenticated
          // loopback socket, after the same well-formedness gate the engine relies on.
          if (isContestSequence(msg.seq) && Number.isSafeInteger(msg.contest)) {
            this.nativeBridge.submitContestClick(pid, msg.contest, msg.seq);
          }
          return;
        }
        this.guestContestClicks.offer(pid, msg, performance.now());
        break;
      }
      case "preview_mana_payment": {
        const session = this.guestSessions.get(pid);
        if (!session) return;
        if (this.eliminatedSeats.has(pid)) {
          void this.send(session, {
            type: "mana_payment_preview_failed",
            requestId: msg.requestId,
            message: "Player has conceded and can no longer act",
          });
          return;
        }
        if (this.gameRunState !== "running") {
          void this.send(session, {
            type: "mana_payment_preview_failed",
            requestId: msg.requestId,
            message: `Game ${this.gameRunState}`,
          });
          return;
        }
        try {
          const sourceIds = this.nativeBridge
            ? await this.nativeBridge.previewManaPayment(msg.action, pid)
            : await this.wasm.previewManaPayment(msg.action, pid);
          void this.send(session, { type: "mana_payment_preview", requestId: msg.requestId, sourceIds });
        } catch (err) {
          void this.send(session, manaPaymentPreviewFailureFrame(msg.requestId, err));
        }
        break;
      }
      case "preview_interaction": {
        const session = this.guestSessions.get(pid);
        // The only silent return, and only because there is no channel to
        // answer on — the sibling arm's shape exactly.
        if (!session) return;
        if (this.eliminatedSeats.has(pid)) {
          void this.send(session, {
            type: "interaction_preview",
            requestId: msg.request.requestId,
            answer: { type: "failed", message: "Player has conceded and can no longer act" },
          });
          return;
        }
        if (this.gameRunState !== "running") {
          void this.send(session, {
            type: "interaction_preview",
            requestId: msg.request.requestId,
            answer: { type: "failed", message: `Game ${this.gameRunState}` },
          });
          return;
        }
        try {
          const preview = this.nativeBridge
            ? await this.nativeBridge.previewInteraction(msg.request, pid)
            : await this.wasm.previewInteraction(msg.request, pid);
          // The requesting session ALONE. No broadcast, no snapshot publish, no
          // AI loop, no persistence — the four calls the "interaction" arm makes
          // and a read-only preview must not.
          void this.send(session, {
            type: "interaction_preview",
            requestId: msg.request.requestId,
            answer: { type: "preview", preview },
          });
        } catch (err) {
          void this.send(session, {
            type: "interaction_preview",
            requestId: msg.request.requestId,
            answer: {
              type: "failed",
              message: err instanceof Error ? err.message : String(err),
            },
          });
        }
        break;
      }
      case "concede": {
        // CR 104.3a: Any player may concede at any time. Route through the
        // engine action so the seat is properly eliminated (CR 800.4a).
        const outcome = await this.concedePlayer(pid, "Player conceded", "conceded");
        if (outcome !== "committed") break;
        // Notify remaining guests with the "conceded" wire variant (not
        // "kicked") so their log entries read correctly.
        for (const [otherPid, s] of this.guestSessions) {
          if (otherPid === pid) continue;
          void this.send(s, {
            type: "player_conceded",
            playerId: pid,
            reason: "Player conceded",
          });
        }
        break;
      }
      case "match_concede": {
        if (!this.boundMatchConcede) {
          if (session) {
            void this.send(session, {
              type: "action_failed",
              message: "Whole-match concession is unavailable for this game",
            });
          }
          return;
        }
        this.requestBoundMatchConcede(pid);
        break;
      }
      // No elimination or run-state gate: the takeback authority decides.
      case "request_takeback": {
        await this.requestTakeback(pid);
        break;
      }
      case "respond_takeback": {
        if (typeof msg.approve !== "boolean") return;
        await this.respondTakeback(pid, msg.approve);
        break;
      }
      case "cancel_takeback": {
        await this.cancelTakeback(pid);
        break;
      }
      default:
        break;
    }
  }

  private handleGuestDisconnect(pid: PlayerId): void {
    if (!this.ownsAuthority()) return;
    if (!this.guestSessions.has(pid)) return;
    if (this.disconnectedSeats.has(pid)) return;

    this.guestSessions.delete(pid);
    this.publishPlayerLatencies();

    if (!this.gameStarted) {
      void this.enqueuePregameOp(async () => {
        if (!this.ownsAuthority()) return;
        if (this.gameStarted) return;
        await this.releaseNativePregameSeat(pid, "disconnect");
        if (!this.ownsAuthority()) return;
        // Pre-game disconnect: free the seat back to the lobby. Drop the token
        // (no reconnect path before game start). The seat number is reused via
        // `nextSeat` rewind so the next joiner takes the same slot.
        this.playerTokens.delete(pid);
        this.guestDecks.delete(pid);
        this.guestNames.delete(pid);
        this.pregameSeatState.seats[pid] = { type: "WaitingHuman" };
        this.pregameSeatState.tokens[pid] = "";
        await this.refreshPregameSeatView();
        this.saveSession();
        this.broadcastSeatSnapshot();
        this.syncLobbyMetadata();
      }).catch((err) => {
        traceAdapter("Host", "disconnect-seat-view-error", {
          error: err instanceof Error ? err.message : String(err),
        });
      });
      return;
    }

    if (this.gameRunState === "terminal") {
      this.disconnectedSeats.set(pid, {
        disconnectedAt: Date.now(),
        timer: null,
      });
      return;
    }

    // Mid-game disconnect: hold the seat open indefinitely. We do NOT
    // auto-concede a dropped player — no grace timer is armed. The game stays
    // `paused-disconnect`, which auto-resumes the moment the player reconnects
    // (see `handleReconnect`'s resume check). Conceding a dropped player is
    // now ALWAYS a deliberate host action ("Continue without them" →
    // `concedeDisconnected`, or `kickPlayer`) — never a timer. CR 104.3a
    // concede still applies, but only on explicit host choice.
    this.disconnectedSeats.set(pid, {
      disconnectedAt: Date.now(),
      timer: null,
    });
    this.gameRunState = "paused-disconnect";
    // A native host's engine authority is the server: it must see this seat go away too,
    // or a running contest would treat the absentee as present and decide it on the
    // ordinary inactivity timeout instead of pausing for the reconnect grace.
    // A concession already in flight owns the seat's socket until it settles.
    if (!this.concedingSeats.has(pid)) this.nativeBridge?.suspendGuest(pid);
    this.announceSeatAway(pid);
  }

  /** Tell every other seat and the host that `pid` is away mid-game, offering the host its choice. */
  private announceSeatAway(pid: PlayerId): void {
    // Notify remaining guests.
    for (const [otherPid, session] of this.guestSessions) {
      if (otherPid === pid) continue;
      void this.send(session, { type: "player_disconnected", playerId: pid });
      void this.send(session, { type: "game_paused", reason: "Player disconnected" });
    }

    this.emit({
      type: "opponentDisconnectedWithChoice",
      playerId: pid,
      gracePeriodMs: this.gracePeriodMs,
    });
    this.emit({ type: "gamePaused", reason: "Player disconnected" });
  }

  private restoreRewoundSeats(snapshot: EngineSnapshot): void {
    // The resume reconcile assumes a cold start; an approval rewinds a live table, so only seats the restored state brings back change.
    const authoritative = new Set(snapshot.state.eliminated_players ?? []);
    let restored = false;
    for (const pid of [...this.eliminatedSeats]) {
      if (authoritative.has(pid)) continue;
      this.eliminatedSeats.delete(pid);
      restored = true;
      if (
        pid === 0
        || !this.playerTokens.has(pid)
        || this.guestSessions.has(pid)
        || this.disconnectedSeats.has(pid)
      ) continue;
      this.armResumeGrace(pid);
      if (this.gameRunState === "running") this.gameRunState = "paused-disconnect";
      this.nativeBridge?.suspendGuest(pid);
      this.announceSeatAway(pid);
    }
    // A resumed host loads the persisted cache as-is.
    if (restored) this.saveSession();
  }

  private async handleReconnect(
    session: PeerSession,
    playerToken: string,
    sessionKey?: P2PSessionKey,
    authority?: P2PAuthorityStamp,
  ): Promise<void> {
    // A resumed host may not publish its state until initialization has
    // persisted the post-resume authority. `pregameReady` resolves only after
    // that barrier; on a failed resume it rejects and the lease is released,
    // so the subsequent authority check closes this unpublishable channel.
    try {
      await this.pregameReady;
    } catch {
      // `abortUnpublishedResume` has already fenced this host. This channel
      // was never admitted, so close it without publishing a resume failure.
      session.close("Host initialization failed");
      return;
    }
    if (!this.ownsAuthority()) {
      this.rejectSuperseded(session);
      return;
    }
    if (
      (sessionKey !== undefined && sessionKey !== this.sessionKey)
      || (authority !== undefined && authority.sessionKey !== this.sessionKey)
    ) {
      void session.send({ type: "reconnect_rejected", reason: "Wrong P2P session" });
      session.close("Wrong P2P session");
      return;
    }
    if (this.kickedTokens.has(playerToken)) {
      void session.send({ type: "reconnect_rejected", reason: "Player kicked" });
      session.close("Kicked");
      return;
    }
    let pid: PlayerId | null = null;
    for (const [seat, token] of this.playerTokens) {
      if (token === playerToken) {
        pid = seat;
        break;
      }
    }
    if (pid === null) {
      void session.send({ type: "reconnect_rejected", reason: "Unknown token" });
      session.close("Unknown token");
      return;
    }
    if (!this.disconnectedSeats.has(pid)) {
      void session.send({
        type: "reconnect_rejected",
        reason: "No grace window active for this seat",
      });
      session.close("Not in grace");
      return;
    }

    if (this.concedingSeats.has(pid)) {
      // A normal close permits retry with the same token after non-commit.
      session.close("Player departure is in progress");
      return;
    }

    if (this.pendingReconnectSessions.has(pid)) {
      void this.send(session, { type: "reconnect_rejected", reason: "Reconnect already in progress" });
      session.close("Reconnect already in progress");
      return;
    }

    // Consume every pre-ACK frame. If a channel sends action/concede/control
    // frames while the snapshot is in flight, those frames must never be
    // replayed into the promoted session later.
    session.onMessage(() => undefined);
    this.pendingReconnectSessions.set(pid, session);
    void this.completeReconnect(pid, session);
  }

  private async completeReconnect(pid: PlayerId, session: PeerSession): Promise<void> {
    await this.enqueueDelivery(() => this.completeReconnectAfterHandoff(pid, session));
  }

  private async completeReconnectAfterHandoff(pid: PlayerId, session: PeerSession): Promise<void> {
    try {
      const handoff = await this.reconnectHandoff(pid);
      if (this.pendingReconnectSessions.get(pid) !== session || !this.ownsAuthority()) return;
      // A native seat handed a Full final view returns to a game that ended while
      // it was away. If the host's terminal never committed (the redial raced the
      // commit, or the commit ended unavailable) it is committed now, from the
      // same recipient-bound Full views, so this seat is given its terminal result
      // instead of a final board with no statement. If the authority is still
      // unavailable the commit reports `terminalUnavailable` and the seat is handed
      // the final board alone, exactly as every attached seat was.
      if (
        this.nativeBridge
        && this.terminalResult === null
        && handoff.snapshot.state.waiting_for.type === "GameOver"
      ) {
        await this.commitTerminalIfComplete(handoff.snapshot, handoff.revision);
        if (this.pendingReconnectSessions.get(pid) !== session || !this.ownsAuthority()) return;
      }
      const terminalReconnect = this.terminalResult !== null;
      if (terminalReconnect) {
        if (
          handoff.snapshot.state.waiting_for.type !== "GameOver"
          || this.terminalResult?.revision !== handoff.revision
        ) {
          this.failPendingReconnect(pid, session, "Reconnect terminal state is unavailable");
          return;
        }
        this.pendingTerminalReconnectAcks.set(pid, {
          session,
          revision: handoff.revision,
          snapshot: handoff.snapshot,
          ready: false,
          stateAckReceived: false,
          finishing: false,
        });
        // Keep the original pending-session consumer; this additional handler
        // admits only the matching final-state ACK until terminal completion.
        session.onMessage((msg) => this.handleGuestMessage(session, msg));
      }

      const acknowledged = await this.send(session, {
        type: "reconnect_ack",
        wireProtocolVersion: WIRE_PROTOCOL_VERSION,
        assignedPlayerId: pid,
        revision: handoff.revision,
        state: handoff.snapshot.state,
        playerNames: this.playerNamesForSeats(),
        ...legalActionsToWire(handoff.snapshot.legalResult),
      });
      // A queued send can be dropped after the reconnect handoff was captured.
      // Only a channel that actually accepted its ACK can take the seat; a
      // failed ACK remains disconnected and is eligible for a later retry.
      if (!acknowledged) {
        this.pendingTerminalReconnectAcks.delete(pid);
        this.failPendingReconnect(pid, session, "Reconnect acknowledgement could not be delivered");
        return;
      }
      if (this.pendingReconnectSessions.get(pid) !== session || !this.ownsAuthority()) return;
      this.seedGuestEntry(pid, handoff.revision);

      if (terminalReconnect) {
        const pending = this.pendingTerminalReconnectAcks.get(pid);
        if (!pending || pending.session !== session) return;
        pending.ready = true;
        this.scheduleTerminalReconnectCompletion(pid, session);
        return;
      }

      if (this.deliveredNativeAiDriverFault !== null) {
        const faultDelivered = await this.send(session, {
          type: "ai_driver_fault",
          ...this.deliveredNativeAiDriverFault,
        });
        if (!faultDelivered) {
          this.failPendingReconnect(pid, session, "Reconnect fault could not be delivered");
          return;
        }
      }
      if (!this.nativeBridge) {
        // The open vote lives in the engine; the native server replays its own.
        const vote = await this.wasm.pendingTakeback();
        if (vote !== null) {
          const replayed = await this.send(session, takebackEventToWire({
            type: "takebackRequested",
            requester: vote.requester,
            requesterName: this.playerNamesForSeats()[vote.requester] ?? "",
            voters: vote.voters,
          }));
          if (!replayed) {
            this.failPendingReconnect(pid, session, "Reconnect takeback request could not be delivered");
            return;
          }
        }
      }
      if (this.pendingReconnectSessions.get(pid) !== session || !this.ownsAuthority()) return;

      const grace = this.disconnectedSeats.get(pid);
      if (!grace) return;
      if (grace.timer !== null) clearTimeout(grace.timer);
      this.pendingReconnectSessions.delete(pid);
      this.disconnectedSeats.delete(pid);
      this.guestSessions.set(pid, session);
      session.onMessage((msg) => this.handleGuestMessage(session, msg));
      this.publishPlayerLatencies();

      await this.tryAcknowledgeNativeTerminal();

      for (const [otherPid, otherSession] of this.guestSessions) {
        if (otherPid !== pid) void this.send(otherSession, { type: "player_reconnected", playerId: pid });
      }
      this.emit({ type: "playerReconnected", playerId: pid });
      this.resumeIfUnblocked();
    } catch (error) {
      this.failPendingReconnect(pid, session, "Reconnect acknowledgement failed");
      traceAdapter("Host", "reconnect-ack-failed", {
        pid,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  /** Capture state, legal actions, and their authority revision as one handoff.
   * WASM has no server revision, so retry if an action fan-out advances the
   * local revision while its viewer snapshot is resolving. */
  private async reconnectHandoff(pid: PlayerId): Promise<{ revision: number; snapshot: EngineSnapshot }> {
    if (this.nativeBridge) {
      // A returning seat reattaches to the engine authority first (it was suspended when
      // its channel dropped), and its handoff is the view that reattachment returned.
      // A retired Full session has no authority left to reattach to; its seat's
      // handoff is the recipient-bound terminal view the bridge holds or, when
      // the reattachment is refused for a retired session, restores.
      if (!this.nativeBridge.isGuestAttached(pid)) {
        const view = await this.nativeBridge.returningSeatView(pid);
        this.recordNativeSeatOutcome(pid, view.revision, { kind: "view", update: { ...view, events: [] } });
      }
      const delivered = this.nativeDeliveredViews.get(pid);
      if (!delivered) throw new AdapterError("P2P_ERROR", `No delivered native snapshot for seat ${pid}`, true);
      return delivered;
    }
    while (this.ownsAuthority()) {
      const revision = this.authoritativeRevision;
      const viewer = await this.wasm.getViewerSnapshot(pid);
      if (revision === this.authoritativeRevision) {
        return { revision, snapshot: { state: viewer.state, legalResult: viewer, seq: nextSnapshotSeq() } };
      }
    }
    throw new AdapterError("P2P_ERROR", "Host session superseded", true);
  }

  private clearPendingReconnectReservation(session: PeerSession): void {
    for (const [pid, pending] of this.pendingReconnectSessions) {
      if (pending === session) {
        this.pendingReconnectSessions.delete(pid);
        this.pendingTerminalReconnectAcks.delete(pid);
        return;
      }
    }
  }

  private failPendingReconnect(pid: PlayerId, session: PeerSession, reason: string): void {
    if (this.pendingReconnectSessions.get(pid) !== session) return;
    this.pendingReconnectSessions.delete(pid);
    this.pendingTerminalReconnectAcks.delete(pid);
    // The seat is still away: if its reattachment to a native engine authority had
    // begun, undo it, so that authority keeps seeing the seat as gone.
    this.nativeBridge?.suspendGuest(pid);
    // An otherwise open guest needs an explicit terminal response; a bare
    // close is treated as a transient transport loss and starts its retry loop.
    // `PeerSession.close` preserves already-queued sends, so this rejection is
    // written before the farewell close frame when the transport remains open.
    void this.send(session, { type: "reconnect_rejected", reason });
    session.close(reason);
  }

  private closePendingReconnect(pid: PlayerId, reason: string): void {
    const session = this.pendingReconnectSessions.get(pid);
    if (!session) return;
    this.pendingReconnectSessions.delete(pid);
    this.pendingTerminalReconnectAcks.delete(pid);
    session.close(reason);
  }

  private resumeIfUnblocked(): void {
    if (
      this.disconnectedSeats.size !== 0
      || this.pendingReconnectSessions.size !== 0
      || (this.gameRunState !== "paused-disconnect" && this.gameRunState !== "paused-manual")
    ) return;
    this.gameRunState = "running";
    for (const session of this.guestSessions.values()) {
      void this.send(session, { type: "game_resumed" });
    }
    this.emit({ type: "gameResumed" });
  }

  /**
   * Concede origin. Distinguishes the three paths that all end at
   * `eliminate_player` so wire broadcasts and local adapter events carry the
   * correct semantic label. CR 104.3a applies uniformly, but UIs need to
   * differentiate "kicked by host" from "left voluntarily" from "host
   * continued past disconnect".
   */
  private async concedePlayer(
    pid: PlayerId,
    reason: string,
    origin: "kick" | "conceded",
  ): Promise<"committed" | "definite_non_commit" | "unknown" | "in_progress" | "inactive"> {
    if (this.disposed || !this.ownsAuthority()) return "inactive";
    if (this.eliminatedSeats.has(pid)) return "inactive";
    const retryingUncertain = this.uncertainConcedeSeats.has(pid);
    if (this.concedingSeats.has(pid) && !retryingUncertain) return "in_progress";
    // Consume the single retry permit before the first await, so an overlapping
    // departure request sees the fence as in progress. Only an outcome that
    // stays unknown restores it.
    this.uncertainConcedeSeats.delete(pid);
    this.concedingSeats.add(pid);
    const retire = () => {
      // Cancel any active grace timer for this seat. `timer` may be null if the
      // host already called `holdForReconnect`.
      const grace = this.disconnectedSeats.get(pid);
      if (grace) {
        if (grace.timer !== null) clearTimeout(grace.timer);
        this.disconnectedSeats.delete(pid);
      }
      this.closePendingReconnect(pid, "Player conceded");
      // Remove the session for self-concede / grace-expiry paths. (The kick
      // path removes its own session before calling concedePlayer so it can
      // send the `kick` wire message first; double-deletion is a no-op here.)
      const session = this.guestSessions.get(pid);
      if (session) {
        this.guestSessions.delete(pid);
        try { session.close("Player conceded"); } catch { /* best-effort */ }
      }
      this.eliminatedSeats.add(pid);
      this.uncertainConcedeSeats.delete(pid);
      this.saveSession();
      void this.tryAcknowledgeNativeTerminal();
    };
    const readAuthoritativeSeat = async () => {
      const bridge = this.nativeBridge;
      if (!bridge) {
        const snapshot = await this.wasm.getSnapshot();
        if (this.disposed || !this.ownsAuthority()) throw new Error("Host authority was superseded");
        const authoritative: NativeViewerUpdate = {
          snapshot,
          events: [],
          revision: this.authoritativeRevision,
        };
        return {
          authoritative,
          views: new Map<PlayerId, NativeSeatReport>([[0, { kind: "view", update: authoritative }]]),
          fanoutAlreadyCompleted: false,
        };
      }
      const authoritative = await bridge.reconcileConcede(pid);
      if (this.disposed || !this.ownsAuthority()) throw new Error("Host authority was superseded");
      const views = bridge.seatReports();
      const fanoutAlreadyCompleted = views.size > 0 && [...views].every(([playerId, report]) =>
        (report.kind === "view"
          ? this.nativeDeliveredViews.get(playerId)?.revision
          : this.nativeWithheldSeats.get(playerId)?.revision) === seatReportRevision(report),
      );
      for (const [playerId, report] of views) {
        const revision = seatReportRevision(report);
        this.recordNativeSeatOutcome(playerId, revision, report);
        this.authoritativeRevision = Math.max(this.authoritativeRevision, revision);
      }
      return { authoritative, views, fanoutAlreadyCompleted };
    };
    // A dropped seat is reattached to the engine authority only to send or
    // reconcile this Concede. When the Concede did not commit it is still away,
    // so the authority must keep seeing it gone: its presence is read from the
    // seat's own socket (contest clock and reconnect grace).
    const suspendIfStillAway = () => {
      if (this.disconnectedSeats.has(pid)) this.nativeBridge?.suspendGuest(pid);
    };
    const settleFromAuthority = async (
      reconciliation: Awaited<ReturnType<typeof readAuthoritativeSeat>>,
    ): Promise<"committed" | "definite_non_commit"> => {
      const { authoritative, views, fanoutAlreadyCompleted } = reconciliation;
      const eliminated = (authoritative.snapshot.state.eliminated_players ?? []).includes(pid);
      const terminal = authoritative.snapshot.state.waiting_for.type === "GameOver";
      if (!eliminated && !terminal) {
        suspendIfStillAway();
        return "definite_non_commit";
      }

      if (eliminated) {
        committed = true;
        retire();
        this.emit(
          origin === "kick"
            ? { type: "playerKicked", playerId: pid, reason }
          : { type: "playerConceded", playerId: pid, reason },
        );
      }
      if (!this.nativeBridge && (eliminated || terminal)) {
        const transition = this.stampBrowserMutation({ events: [], log_entries: [] }, true);
        await this.publishHostSnapshot(transition.result);
        await this.broadcastStateUpdate(transition, reason);
        await this.runAiLoop();
      } else if (terminal) {
        try {
          const hostReport = views.get(0);
          if (!hostReport) throw new Error("Full terminal host view is unavailable");
          if (!fanoutAlreadyCompleted) {
            // A2 restored the complete recipient views outside the ordinary
            // revision barrier. Run them through the same state fan-out so
            // each P2P seat receives final state before its terminal statement.
            await this.handleNativeRevision(authoritative.revision, views, reason);
          } else if (hostReport.kind === "view") {
            await this.commitTerminalIfComplete(hostReport.update.snapshot, authoritative.revision, reason);
          } else {
            await this.commitWithheldHostTerminal(authoritative.revision);
          }
        } catch {
          this.emit({
            type: "terminalUnavailable",
            message: i18n.t("multiplayer:terminalRecovery.finalViewsUnpublished"),
          });
        }
      }
      if (eliminated) {
        void this.persistAuthoritativeState();
        return "committed";
      }
      suspendIfStillAway();
      return "definite_non_commit";
    };
    let committed = false;
    let keepFence = false;
    try {
      this.closePendingReconnect(pid, "Player departure is in progress");
      if (retryingUncertain) {
        let reconciliation: Awaited<ReturnType<typeof readAuthoritativeSeat>>;
        try {
          reconciliation = await readAuthoritativeSeat();
        } catch {
          keepFence = true;
          this.uncertainConcedeSeats.add(pid);
          // The send or the failed reconciliation may have reattached the seat; it is
          // still away, so the engine authority must keep seeing it gone while fenced.
          suspendIfStillAway();
          this.emit({
            type: "error",
            message: i18n.t("multiplayer:terminalRecovery.concedeUnreconciled"),
          });
          return "unknown";
        }
        const outcome = await settleFromAuthority(reconciliation);
        if (
          outcome === "committed"
          || reconciliation.authoritative.snapshot.state.waiting_for.type === "GameOver"
        ) return outcome;
        // This invocation is a later explicit host/player departure request.
        // The fresh Full barrier proved the earlier Action did not commit, so
        // it is now safe to submit this new Concede once.
      }
      const concedeAction = {
        type: "Concede",
        data: { player_id: pid },
      } as unknown as GameAction;
      // Concede's engine guard requires `actor === player_id`. `pid` is both
      // the seat being conceded and the authenticated identity we're acting
      // on behalf of (e.g. grace-expiry or kick).
      const transition = await this.applyBrowserMutation(() => this.nativeBridge
        ? this.nativeBridge.submitAction(concedeAction, pid)
        : this.wasm.submitAction(concedeAction, pid));
      if (this.disposed || !this.ownsAuthority()) return "inactive";
      committed = true;
      retire();
      // Both host emissions precede the fan-out: the concession has applied,
      // and a guest link failure must not hide it from the host's own screen
      // (#7924). Order between them is unchanged — state, then the notice.
      await this.publishHostSnapshot(transition.result);
      this.emit(
        origin === "kick"
          ? { type: "playerKicked", playerId: pid, reason }
          : { type: "playerConceded", playerId: pid, reason },
      );
      await this.broadcastStateUpdate(transition, reason);
      await this.runAiLoop();
      void this.persistAuthoritativeState();
    } catch (err) {
      if (this.disposed || !this.ownsAuthority()) return "inactive";
      console.warn("[P2PHost] Concede settlement is uncertain; checking the authoritative seat");
      if (!committed) {
        if (
          err instanceof AdapterError
          && (
            err.code === AdapterErrorCode.ACTION_NOT_SENT
            || isActionRejection(err.rejection)
          )
        ) {
          suspendIfStillAway();
          this.resumeIfUnblocked();
          return "definite_non_commit";
        }
        let reconciliation: Awaited<ReturnType<typeof readAuthoritativeSeat>>;
        try {
          reconciliation = await readAuthoritativeSeat();
        } catch {
          keepFence = true;
          this.uncertainConcedeSeats.add(pid);
          // The send or the failed reconciliation may have reattached the seat; it is
          // still away, so the engine authority must keep seeing it gone while fenced.
          suspendIfStillAway();
          this.emit({
            type: "error",
            message: i18n.t("multiplayer:terminalRecovery.concedeUnreconciled"),
          });
          return "unknown";
        }
        const outcome = await settleFromAuthority(reconciliation);
        if (outcome === "definite_non_commit") return outcome;
      }
    } finally {
      if (!keepFence) {
        this.concedingSeats.delete(pid);
        this.uncertainConcedeSeats.delete(pid);
        if (!this.disposed && this.ownsAuthority()) this.resumeIfUnblocked();
      }
    }
    return committed ? "committed" : "unknown";
  }

  // ────────────────────────────────────────────────────────────────────────
  // Public host-only controls (called by UI components).
  // ────────────────────────────────────────────────────────────────────────

  /**
   * Forcibly remove a player from the game. CR 104.3a: kicked players forfeit.
   * Adds the seat's token to the denylist so they cannot reconnect.
   */
  async kickPlayer(pid: PlayerId, reason: string = "Kicked by host"): Promise<void> {
    if (!this.ownsAuthority()) return;
    if (this.concedingSeats.has(pid) && !this.uncertainConcedeSeats.has(pid)) return;
    const token = this.playerTokens.get(pid);
    if (token) this.kickedTokens.add(token);
    this.closePendingReconnect(pid, "Kicked");
    // Persist the kick before the session close — the kickedTokens set
    // survives host reload so a kicked guest can't sneak back in on
    // resume.
    this.saveSession();
    // Remove session BEFORE concedePlayer so we can send the `kick` wire
    // message on the way out; concedePlayer's own session-cleanup is a no-op
    // for an already-removed seat.
    const session = this.guestSessions.get(pid);
    if (session) {
      void this.send(session, { type: "kick", reason });
      try { session.close("Kicked"); } catch { /* best-effort */ }
      this.guestSessions.delete(pid);
    }
    const outcome = await this.concedePlayer(pid, reason, "kick");
    if (outcome !== "committed") return;
    // Broadcast kick to remaining guests (concedePlayer emits playerKicked
    // locally; remaining peers need the wire message).
    for (const [otherPid, s] of this.guestSessions) {
      if (otherPid === pid) continue;
      void this.send(s, { type: "player_kicked", playerId: pid, reason });
    }
  }

  /**
   * Continue the game without the disconnected player (auto-concede).
   * Cancels their grace timer and routes to `concedePlayer`.
   */
  async concedeDisconnected(pid: PlayerId): Promise<void> {
    if (!this.ownsAuthority()) return;
    const reason = "Host continued without reconnecting player";
    const outcome = await this.concedePlayer(pid, reason, "conceded");
    if (outcome !== "committed") return;
    for (const [otherPid, s] of this.guestSessions) {
      if (otherPid === pid) continue;
      void this.send(s, { type: "player_conceded", playerId: pid, reason });
    }
  }

  /**
   * Convert an active "paused-disconnect" into "paused-manual" — cancels the
   * grace timer so the game waits indefinitely for the player to reconnect.
   * The `disconnectedSeats` entry is preserved so the reconnect path still
   * fires; only the auto-concede timer is cancelled.
   */
  holdForReconnect(pid: PlayerId): void {
    if (!this.ownsAuthority()) return;
    const grace = this.disconnectedSeats.get(pid);
    if (grace) {
      if (grace.timer !== null) clearTimeout(grace.timer);
      // Null out the timer field (typed `Timer | null`). The reconnect handler
      // branches on null-or-not before calling `clearTimeout`.
      this.disconnectedSeats.set(pid, {
        disconnectedAt: grace.disconnectedAt,
        timer: null,
      });
    }
    this.gameRunState = "paused-manual";
  }

  /** Manually pause (host UI). */
  requestPause(): void {
    if (!this.ownsAuthority()) return;
    if (this.gameRunState === "running") {
      this.gameRunState = "paused-manual";
      for (const [, s] of this.guestSessions) {
        void this.send(s, { type: "game_paused", reason: "Paused by host" });
      }
      this.emit({ type: "gamePaused", reason: "Paused by host" });
    }
  }

  /** Manually resume only an explicit manual pause after every reconnect settles. */
  requestResume(): void {
    if (!this.ownsAuthority()) return;
    if (
      this.gameRunState === "paused-manual" &&
      this.disconnectedSeats.size === 0 &&
      this.pendingReconnectSessions.size === 0
    ) {
      this.gameRunState = "running";
      for (const [, s] of this.guestSessions) {
        void this.send(s, { type: "game_resumed" });
      }
      this.emit({ type: "gameResumed" });
    }
  }
}

/**
 * How a parked guest submission is being settled. A typed union rather than a
 * pair of methods so that BOTH arms are forced through the single settlement
 * path that clears the slot handles and the submission timeout together.
 */
type PendingSubmissionOutcome =
  | { kind: "resolve"; result: SubmitResult }
  | { kind: "reject"; error: Error };

/**
 * Guest-side P2P adapter. Maintains the `Peer` reference for auto-reconnect,
 * persists session token to `sessionStorage` (via `p2pSession` service), and
 * applies host-broadcasted state updates locally.
 */
export class P2PGuestAdapter implements EngineAdapter {
  /**
   * The single cached engine pair, rebuilt (and re-stamped) once per inbound
   * state-bearing message — `game_setup`, `reconnect_ack`, `state_update`.
   * `getState`/`getLegalActions` both read from THIS object, so they can no
   * longer straddle two updates the way two independently-cached fields could.
   * The host's ordered DataChannel delivers updates in engine order, so
   * stamping on arrival reproduces that order exactly.
   */
  private snapshot: EngineSnapshot | null = null;
  private listeners: P2PAdapterEventListener[] = [];
  private pendingResolve: ((result: SubmitResult) => void) | null = null;
  private pendingReject: ((error: Error) => void) | null = null;
  /** Armed with the slot in `parkPendingSubmission`, cleared only by
   *  `settlePendingSubmission` — see `SUBMISSION_TIMEOUT_MS`. */
  private pendingSubmissionTimer: ReturnType<typeof setTimeout> | null = null;
  private nextManaPaymentPreviewRequestId = 1;
  private pendingManaPaymentPreviews = new Map<
    number,
    { resolve: (sourceIds: ObjectId[]) => void; reject: (error: Error) => void }
  >();
  private pendingInteractionPreviews = new Map<
    string,
    { resolve: (preview: InteractionPreview) => void; reject: (error: Error) => void }
  >();
  private session: PeerSession | null = null;
  private hostLatencies: Record<number, number | null> = {};
  /** The current transport becomes authenticated only after its setup ACK. */
  private authenticatedSession: PeerSession | null = null;
  private playerToken: string | null = null;
  /**
   * Undeliverable inbound frames since a frame decoded past
   * `handleHostMessage`'s unauthenticated-discard guard, which is the sole
   * reset point. Bound to the adapter, not the session, because the frame that
   * exhausts the budget arrives on the session the first close created. It is
   * deliberately NOT reset in `attachSession`: every retry re-enters that
   * method, which would make the bound inert.
   */
  private undeliverableFramesSinceDecode = 0;
  private assignedPlayerId: PlayerId | null = null;
  /** Current host lease accepted from game_setup/reconnect_ack. */
  private authority: P2PAuthorityStamp | null = null;
  readonly supportsMatchConcede: true | undefined;
  readonly supportsTakeback = true;
  private matchConcedeSent = false;
  /** Revision of the cached state frame. A terminal result is bound to this
   * exact final state, not merely to the room code. */
  private cachedRevision: number | null = null;
  private withheldState: StateWithheldNotice | null = null;
  private readonly acceptedAiDriverFaultIds = new Set<number>();
  /**
   * Once true, the adapter is in a terminal state (kicked, reconnect rejected,
   * or disposed). `handleHostDisconnect` bails out so the auto-reconnect loop
   * does NOT fire — preventing a kicked guest from spinning ~30s of backoff
   * attempts against a token they'll never be accepted with.
   */
  private terminated = false;

  // Promise resolved on game_setup OR reconnect_ack, whichever arrives first.
  // Reconnecting guests take the `reconnect_ack` path, so `initializeGame()`
  // must resolve there too or it will hang indefinitely.
  private gameSetupPromise: Promise<SubmitResult>;
  private gameSetupResolve!: (result: SubmitResult) => void;
  private gameSetupReject!: (error: Error) => void;
  private gameSetupSettled = false;

  constructor(
    private readonly deckData: unknown,
    private readonly hostPeer: TransportPeer,
    private readonly hostPeerId: string,
    private readonly initialConn: TransportConnection,
    existingPlayerToken?: string,
    private readonly displayName?: string,
    private readonly reservationToken?: string,
    // IndexedDB key for the persisted reconnect token, decoupled from
    // `hostPeerId` (the dial target). The dial target tracks the live
    // PEER_ID_PREFIX; the storage key is held on the legacy prefix so tokens
    // persisted before a prefix bump still resolve. Falls back to
    // `hostPeerId` when omitted (callers that don't persist across bumps).
    private readonly sessionKey?: string,
    existingAuthority?: P2PAuthorityStamp,
    matchConcedeBound: boolean = false,
  ) {
    if (existingPlayerToken) {
      this.playerToken = existingPlayerToken;
    }
    this.authority = existingAuthority ?? null;
    this.supportsMatchConcede = matchConcedeBound ? true : undefined;
    this.gameSetupPromise = new Promise<SubmitResult>((resolve, reject) => {
      this.gameSetupResolve = resolve;
      this.gameSetupReject = reject;
    });
  }

  onEvent(listener: P2PAdapterEventListener): () => void {
    this.listeners.push(listener);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener);
    };
  }

  private emit(event: P2PAdapterEvent): void {
    for (const listener of this.listeners) {
      listener(event);
    }
  }

  async initialize(): Promise<void> {
    traceAdapter("Guest", "initialize-start", { hasPlayerToken: Boolean(this.playerToken) });
    this.attachSession(this.initialConn);
    if (this.playerToken) {
      traceAdapter("Guest", "send-reconnect", { hostPeerId: this.hostPeerId });
      this.send({
        type: "reconnect",
        playerToken: this.playerToken,
        wireProtocolVersion: WIRE_PROTOCOL_VERSION,
        ...(this.authority ? { sessionKey: this.authority.sessionKey } : {}),
      });
    } else {
      traceAdapter("Guest", "send-guest-deck", { hostPeerId: this.hostPeerId });
      this.send({
        type: "guest_deck",
        deckData: this.deckData,
        displayName: this.displayName,
        reservationToken: this.reservationToken,
        wireProtocolVersion: WIRE_PROTOCOL_VERSION,
      });
    }
  }

  private attachSession(conn: TransportConnection): void {
    if (this.terminated) {
      conn.close();
      return;
    }
    traceAdapter("Guest", "attach-session", { connOpen: conn.open });
    const session = createPeerSession(conn, {
      onLatency: (latencyMs) => {
        if (this.session !== session || latencyMs !== null) return;
        this.hostLatencies = Object.fromEntries(Object.keys(this.hostLatencies).map((pid) => [pid, null]));
        this.emit({ type: "playerLatencies", latencies: this.hostLatencies });
      },
      onSessionEnd: () => {
        this.handleHostDisconnect(session);
      },
      onUndeliverableFrame: () => {
        if (this.session !== session || this.terminated) return;
        // A seated guest holds a token too, so preserve its existing drop
        // policy before the first-contact retry logic below. The host resends
        // state updates and terminal results; preview replies are not covered
        // by that redelivery and their timeout policy is a separate concern.
        if (this.authenticatedSession === session) return;
        this.undeliverableFramesSinceDecode += 1;
        // `reconnect_ack` IS re-requestable — `attemptReconnect` re-sends
        // `reconnect` — so a token-bearing guest spends exactly one retry.
        // `game_setup` is not re-requestable, and three of the causes here
        // (unknown envelope byte, unknown type from a newer host, non-binary
        // frame from an older bundle) are persistent, so re-dialling on the
        // next one would hang silently forever instead of settling the waiter
        // or surfacing the failure.
        if (this.playerToken && this.undeliverableFramesSinceDecode === 1) {
          session.close("Undecodable frame during reconnect");
          return;
        }
        const reason = i18n.t("multiplayer:reconnectRejected.frameUndecodable");
        this.terminate();
        this.rejectGameSetup(reason);
        this.emit({ type: "reconnectFailed", reason });
      },
    });
    this.rejectPendingSubmission(
      new AdapterError("P2P_ERROR", "Host disconnected while submitting an action", true),
    );
    this.rejectPendingManaPaymentPreviews(
      new AdapterError("P2P_ERROR", "Host disconnected during mana-payment preview", true),
    );
    this.rejectPendingInteractionPreviews(
      new AdapterError("P2P_ERROR", "Host disconnected during interaction preview", true),
    );
    this.session = session;
    this.authenticatedSession = null;
    this.matchConcedeSent = false;
    session.onMessage((msg) => this.handleHostMessage(session, msg));
  }

  async initializeGame(): Promise<SubmitResult> {
    return this.gameSetupPromise;
  }

  /** One click in the running contest; the host stamps it on arrival. Never queued. */
  submitContestClick(contest: number, seq: number): boolean {
    if (this.session && this.assignedPlayerId !== null) {
      this.send({ type: "contest_click", contest, seq });
    }
    return true;
  }

  async submitAction(action: GameAction, _actor: PlayerId): Promise<SubmitResult> {
    // `_actor` is unused: the host re-tags the incoming action with the
    // PlayerId bound to this WebRTC session at join time. `senderPlayerId`
    // on the wire is kept for the host's envelope-level sanity check
    // (rejects early with a clear diagnostic) but is NEVER used by the host
    // as the engine `actor`. If this client were malicious and claimed
    // another identity, the host would detect the mismatch and drop the
    // action before touching the engine.
    this.requireAuthenticatedSession();
    return new Promise<SubmitResult>((resolve, reject) => {
      this.parkPendingSubmission(resolve, reject);
      this.send({
        type: "action",
        senderPlayerId: this.assignedPlayerId!,
        action,
      });
    });
  }

  async submitInteraction(
    submission: InteractionSubmission,
    _actor: PlayerId,
  ): Promise<SubmitResult> {
    this.requireAuthenticatedSession();
    return new Promise<SubmitResult>((resolve, reject) => {
      this.parkPendingSubmission(resolve, reject);
      this.send({
        type: "interaction",
        senderPlayerId: this.assignedPlayerId!,
        submission,
      });
    });
  }

  async previewManaPayment(action: GameAction, _actor: PlayerId): Promise<ObjectId[]> {
    this.requireAuthenticatedSession();

    const requestId = this.nextManaPaymentPreviewRequestId++;
    return new Promise<ObjectId[]>((resolve, reject) => {
      this.pendingManaPaymentPreviews.set(requestId, { resolve, reject });
      this.send({ type: "preview_mana_payment", requestId, action });
    });
  }

  async previewInteraction(
    request: InteractionPreviewRequest,
    _actor: PlayerId,
  ): Promise<InteractionPreview> {
    this.requireAuthenticatedSession();

    return new Promise<InteractionPreview>((resolve, reject) => {
      this.pendingInteractionPreviews.set(request.requestId, { resolve, reject });
      // `request` is forwarded VERBATIM — no field is read, reshaped or rebuilt.
      this.send({ type: "preview_interaction", request });
    });
  }

  async getState(): Promise<GameState> {
    if (!this.snapshot) {
      throw new AdapterError("P2P_ERROR", "No game state available", false);
    }
    return this.snapshot.state;
  }

  async getLegalActions(): Promise<LegalActionsResult> {
    return this.snapshot?.legalResult ?? EMPTY_LEGAL_ACTIONS;
  }

  async getSnapshot(): Promise<EngineSnapshot> {
    if (!this.snapshot) {
      throw new AdapterError("P2P_ERROR", "No game state available", false);
    }
    return this.snapshot;
  }

  /** Rebuild the cached pair from an inbound state-bearing message, stamping
   *  it with a fresh globally-monotonic seq at arrival. */
  private cacheSnapshot(state: GameState, legalResult: LegalActionsResult): EngineSnapshot {
    this.snapshot = { state, legalResult, seq: nextSnapshotSeq() };
    return this.snapshot;
  }

  /** Report the highest revision this guest has actually APPLIED.
   *
   *  Called from every arm that accepts a state-bearing frame, and from the
   *  stale-drop branch of `state_update`. A sender-side ledger records
   *  TRANSMISSION — `send()` resolving true means the bytes reached the
   *  channel, not that the guest ever applied them — so this is the only
   *  frame that reports application. On the stale-drop branch `cachedRevision`
   *  is still the NEWER revision the guest already holds, not the stale one it
   *  just discarded, so the ack additionally reports being ahead of a resend.
   *
   *  Placement is NOT uniform and rests on no single criterion. `emit` does
   *  not wrap its listeners in try/catch (unlike `peer.ts`, which wraps each
   *  handler), so an ack sitting after an `emit` is suppressed by a throwing
   *  subscriber — and whether that suppression is WANTED differs by arm.
   *   - `state_update`: ack right after `cacheSnapshot`, before the emit. The
   *     guest holds the state and serves `getState()` from there, so the ack
   *     reports what the guest HAS, not what its UI has rendered. Acking after
   *     the emit would be worse than merely late: a resent EQUAL revision is
   *     not `<` the cached one, so it re-enters this same arm and throws
   *     again, and since the host drives redelivery off these acks that is a
   *     resend loop rather than a heal.
   *   - `game_setup` / `reconnect_ack`: ack AFTER `settleGameSetup`. Until
   *     that promise settles `initializeGame()` has not resolved and the guest
   *     cannot act at all, so here suppression is the wanted outcome: an
   *     unacked seat is the honest report. Two limits on that, both real —
   *     it covers only the setup frame itself, since a later `state_update`
   *     acks from its own arm with no `gameSetupSettled` check; and a
   *     redelivery rescues only a TRANSIENT throw, because a deterministic one
   *     re-enters the same arm and throws again.
   *
   *  No-op when `cachedRevision` is null. Every host sender stamps `revision`
   *  today, but the frame types declare it optional, so an unstamped
   *  state-bearing frame would be applied and NOT acked, with no warning.
   *
   *  Authentication: all four call sites are on an authenticated session by
   *  the time they reach here, but by two different routes.
   *  `handleHostMessage`'s top-level guard already requires
   *  `authenticatedSession === session` for every type except `game_setup`,
   *  `reconnect_ack`, `reconnect_rejected`, `kick` and `host_left` — so the
   *  two `state_update` sites are gated there before their arm is entered.
   *  The other two arms are exempt from that guard precisely because they are
   *  what ASSIGNS `authenticatedSession`, and each does so before its ack.
   *
   *  `send()` stamps `this.authority` when the guest holds one, so an ack
   *  normally runs the host's `hasExactP2PAuthority` supersede check whose
   *  failure path calls `rejectSuperseded` and closes the session. That is the
   *  identical path an `action` already takes, so it is believed benign — but
   *  it does mean this purely informational frame is capable of ending a
   *  session, which is stated here rather than left latent. It also changes
   *  the TIMING of that outcome: an `action` is user-driven, while an ack
   *  rides every accepted broadcast, so a guest on a stale stamp would be
   *  rejected on the next broadcast rather than on its next action. Both setup
   *  arms refresh `this.authority` from the host's own frame before their ack
   *  — conditionally, but the host stamps every outbound frame — so this is
   *  not believed reachable.
   */
  private sendStateAck(): void {
    if (this.cachedRevision === null) return;
    this.send({ type: "state_ack", revision: this.cachedRevision });
  }

  /** A delivered state ends the withheld condition. */
  private settleWithheld(): void {
    if (!this.withheldState) return;
    this.withheldState = null;
    this.emit({ type: "stateDelivery", withheld: null });
  }

  private async acceptTerminalResult(
    session: PeerSession,
    message: Extract<P2PMessage, { type: "terminal_result" }>,
  ): Promise<void> {
    if (session !== this.session || this.authenticatedSession !== session || this.terminated) return;
    const { result } = message;
    if (
      !isValidP2PTerminalResult(result)
      || !message.authority
      || message.authority.sessionKey !== result.lease.sessionKey
      || message.authority.hostIncarnation !== result.lease.hostIncarnation
      || this.authority === null
      || result.key !== this.authority.sessionKey
      || result.lease.hostIncarnation !== this.authority.hostIncarnation
      || result.recipient !== this.assignedPlayerId
      || this.cachedRevision !== result.revision
      || this.snapshot?.state.waiting_for.type !== "GameOver"
    ) {
      this.emit({ type: "terminalUnavailable", message: "Rejected an unbound P2P terminal result" });
      return;
    }
    try {
      if ((await p2pFinalStateCommitment(this.snapshot.state)) !== result.finalStateCommitment) {
        this.emit({ type: "terminalUnavailable", message: "P2P terminal result did not match the final state" });
        return;
      }
      if (session !== this.session || this.authenticatedSession !== session || this.terminated) return;
      if (!(await commitP2PTerminalResult(result))) {
        this.emit({ type: "terminalUnavailable", message: "Conflicting P2P terminal result" });
        return;
      }
    } catch (error) {
      this.emit({
        type: "terminalUnavailable",
        message: error instanceof Error ? error.message : "Failed to retain P2P terminal result",
      });
      return;
    }
    if (session !== this.session || this.authenticatedSession !== session || this.terminated) return;
    if (!(await this.send({ type: "terminal_result_receipt", result }))) {
      this.emit({ type: "terminalUnavailable", message: i18n.t("multiplayer:terminalRecovery.receiptUnsent") });
      return;
    }
    if (session !== this.session || this.authenticatedSession !== session || this.terminated) return;
    this.terminate();
    void clearP2PSession(this.sessionKey ?? this.hostPeerId);
    this.emit({ type: "terminalResult", result });
  }

  restoreState(_state: PersistedGameState): void {
    throw new AdapterError("P2P_ERROR", "Undo not supported in P2P games", false);
  }

  estimateBracket(_deck: BracketDeckRequest): Promise<BracketEstimate | null> {
    throw new AdapterError(
      AdapterErrorCode.BRACKET_ESTIMATION_UNSUPPORTED,
      "Bracket estimation is a local feature; not available in P2P sessions.",
      false,
    );
  }

  sendConcede(): void {
    if (!this.currentAuthenticatedSession()) return;
    this.send({ type: "concede" });
  }

  sendRequestTakeback(): void {
    if (!this.currentAuthenticatedSession()) return;
    this.send({ type: "request_takeback" });
  }

  sendRespondTakeback(approve: boolean): void {
    if (!this.currentAuthenticatedSession()) return;
    this.send({ type: "respond_takeback", approve });
  }

  sendCancelTakeback(): void {
    if (!this.currentAuthenticatedSession()) return;
    this.send({ type: "cancel_takeback" });
  }

  /** Requests settlement from the authenticated host-side match authority. */
  sendMatchConcede(): void {
    if (!this.currentAuthenticatedSession()) return;
    if (!this.supportsMatchConcede || this.matchConcedeSent) return;
    this.matchConcedeSent = true;
    this.send({ type: "match_concede" });
  }

  dispose(): void {
    // Mark terminal BEFORE closing the session so the session's
    // `onSessionEnd` → `handleHostDisconnect` short-circuit fires and skips
    // the auto-reconnect loop.
    this.terminate(new AdapterError("P2P_ERROR", "Adapter disposed", true));
    try {
      this.hostPeer.destroy();
    } catch {
      /* best-effort */
    }
    this.snapshot = null;
    this.listeners = [];
  }

  private handleHostMessage(session: PeerSession, msg: P2PMessage): void {
    if (session !== this.session) return;
    if (this.terminated) return;
    if (
      this.authenticatedSession !== session
      && msg.type !== "game_setup"
      && msg.type !== "reconnect_ack"
      && msg.type !== "reconnect_rejected"
      && msg.type !== "kick"
      // The host broadcasts this to both joined and still-lobbying guests
      // before it closes their sessions, so it must stop reconnecting even
      // before this connection has delivered its setup handshake.
      && msg.type !== "host_left"
    ) {
      return;
    }
    // Reset HERE, not at this function's entry: a decodable frame this guest
    // cannot use (a `state_update` before authentication) reaches the entry and
    // dies at the guard above, so it is no evidence that the decode path is
    // readable.
    this.undeliverableFramesSinceDecode = 0;
    traceAdapter("Guest", "host-message", { type: msg.type });
    // First-contact protocol-version check. `game_setup` and `reconnect_ack`
    // both carry `wireProtocolVersion`; if a future host bumps the version
    // and the guest tab is running the older bundle (or vice versa), this
    // is the in-band signal that lets us surface "refresh both windows"
    // instead of silently corrupting state via field-shape drift. The
    // PEER_ID_PREFIX bump prevents *room discovery* across mismatched
    // bundles, but a same-version-prefix-different-message-shape change
    // would slip past it — that's what this guards.
    //
    // This is the SOLE guest-side enforcement point for the rule.
    // `validateMessage` once checked it too, one layer lower: it threw from
    // inside `decodeWireMessage`, and `peer.ts` warns-and-drops on a decode
    // throw, so the frame died in the transport and this branch was dead code
    // for the case it was written for. A lower layer cannot surface the
    // mismatch — rejecting `gameSetupPromise` and emitting `reconnectFailed`
    // both need adapter state the transport has no access to.
    if (msg.type === "game_setup" || msg.type === "reconnect_ack") {
      // Read through a widened local: these two messages DECLARE the field at
      // the literal `typeof WIRE_PROTOCOL_VERSION`, so comparing `msg.…`
      // directly narrows `msg` to `never` in the mismatch branch and the
      // interpolation below stops compiling. The runtime value is whatever the
      // host actually sent, which is the entire point of the check.
      const hostVersion: number = msg.wireProtocolVersion;
      if (hostVersion !== WIRE_PROTOCOL_VERSION) {
        // This reason reaches the user RAW: rejectGameSetup → AdapterError
        // → GameProvider's error toast. `i18n/README.md` asks for `t()` on
        // frontend-authored strings; this path predates that and is left as
        // found — the violation is pre-existing and out of scope here.
        const reason = `Wire protocol mismatch: host sent v${hostVersion}, this client speaks v${WIRE_PROTOCOL_VERSION}. Refresh both windows.`;
        console.error("[P2PGuestAdapter]", reason);
        this.terminate();
        this.rejectGameSetup(reason);
        this.emit({ type: "reconnectFailed", reason });
        return;
      }
      if (msg.authority !== undefined && !isP2PAuthorityStamp(msg.authority)) {
        const reason = "Host sent a malformed P2P authority";
        this.terminate();
        this.rejectGameSetup(reason);
        this.emit({ type: "reconnectFailed", reason });
        return;
      }
    }
    if (!this.acceptsHostAuthority(msg)) return;
    switch (msg.type) {
      case "player_latencies": {
        if (typeof msg.latencies !== "object" || msg.latencies === null || Array.isArray(msg.latencies)) return;
        const entries = Object.entries(msg.latencies);
        if (entries.some(([pid, ms]) => !Number.isSafeInteger(Number(pid)) || Number(pid) < 0
          || (ms !== null && (typeof ms !== "number" || !Number.isFinite(ms) || ms < 0)))) return;
        this.hostLatencies = msg.latencies;
        this.emit({ type: "playerLatencies", latencies: this.hostLatencies });
        break;
      }
      case "game_setup": {
        this.authenticatedSession = session;
        this.assignedPlayerId = msg.assignedPlayerId;
        this.playerToken = msg.playerToken;
        if (isP2PAuthorityStamp(msg.authority)) {
          this.authority = msg.authority;
          void saveP2PSession(this.sessionKey ?? this.hostPeerId, {
            playerToken: msg.playerToken,
            playerId: msg.assignedPlayerId,
            authority: this.authority,
          });
        }
        this.cachedRevision = msg.revision ?? null;
        this.cacheSnapshot(msg.state, legalActionsFromWire(msg));
        this.settleWithheld();
        this.emit({ type: "playerIdentity", playerId: msg.assignedPlayerId, playerNames: msg.playerNames });
        this.settleGameSetup({ events: msg.events });
        this.sendStateAck();
        break;
      }
      case "reconnect_ack": {
        this.authenticatedSession = session;
        this.assignedPlayerId = msg.assignedPlayerId;
        if (this.playerToken && isP2PAuthorityStamp(msg.authority)) {
          this.authority = msg.authority;
          void saveP2PSession(this.sessionKey ?? this.hostPeerId, {
            playerToken: this.playerToken,
            playerId: msg.assignedPlayerId,
            authority: this.authority,
          });
        }
        this.cachedRevision = msg.revision ?? null;
        const reconnectSnapshot = this.cacheSnapshot(msg.state, legalActionsFromWire(msg));
        this.settleWithheld();
        this.emit({ type: "bootstrapDelivered" });
        this.emit({ type: "playerIdentity", playerId: msg.assignedPlayerId, playerNames: msg.playerNames });
        this.emit({
          type: "stateChanged",
          snapshot: reconnectSnapshot,
          events: [],
        });
        // Resolve `initializeGame()` for the reconnect path too. Reconnecting
        // guests never receive `game_setup`; without this they would hang.
        // Post-reconnect `reconnect_ack` messages (guest briefly disconnects
        // a second time) are idempotent — the `gameSetupSettled` guard
        // prevents double-resolution.
        this.settleGameSetup({ events: [] });
        this.sendStateAck();
        break;
      }
      case "reconnect_rejected": {
        const reason = reconnectRejectionReason(msg);
        this.terminate();
        this.rejectGameSetup(reason);
        this.emit({ type: "reconnectFailed", reason });
        this.emit({ type: "gameOver", winner: null, reason });
        break;
      }
      case "kick": {
        this.terminate();
        const kickFormat = (msg as { format?: string }).format;
        const isDeckRejection = msg.reason.startsWith("Deck rejected:");
        this.rejectGameSetup(
          kickFormat ? `${msg.reason}||format:${kickFormat}` : msg.reason,
        );
        if (!isDeckRejection) {
          this.emit({ type: "gameOver", winner: null, reason: msg.reason });
        }
        break;
      }
      case "host_left": {
        this.terminate();
        this.rejectGameSetup(msg.reason);
        this.emit({ type: "gameOver", winner: null, reason: msg.reason });
        break;
      }
      case "terminal_result": {
        void this.acceptTerminalResult(session, msg);
        break;
      }
      case "takeback_requested":
      case "takeback_resolved":
      case "request_rejected": {
        this.emit(takebackEventFromWire(msg));
        break;
      }
      case "ai_driver_fault": {
        if (this.acceptedAiDriverFaultIds.has(msg.id)) break;
        if (this.cachedRevision === null || this.cachedRevision < msg.revision) {
          this.emit({ type: "terminalUnavailable", message: "Rejected an out-of-order native AI driver fault" });
          break;
        }
        this.acceptedAiDriverFaultIds.add(msg.id);
        const error = new AdapterError("P2P_ERROR", msg.message, false);
        this.terminate(error);
        this.emit({ type: "error", message: msg.message });
        break;
      }
      case "state_update": {
        if (this.authenticatedSession !== session) return;
        // PeerJS normally preserves message order, but state resync after a
        // reconnect can race a previously queued delivery. Never let that old
        // view overwrite a newer authority revision: it can leave two peers
        // each waiting for the other to act.
        //
        // The STRICT `<` is load-bearing, not stylistic: the host's redelivery
        // sweep resends at its OWN `authoritativeRevision`, which equals this
        // guest's cached revision whenever the terminal clause alone nominated
        // the seat (a seat current on state but still owing a statement), and
        // is strictly higher in the lag case. That equal-revision frame must
        // fall through to settle `pendingResolve` below and produce a fresh
        // ack. Widening this to `<=` would drop exactly the frame that breaks
        // a stalled submission — silently reintroducing the deadlock the
        // acceptance ledger exists to end.
        if (
          msg.revision !== undefined
          && this.cachedRevision !== null
          && msg.revision < this.cachedRevision
        ) {
          // Ack the revision the guest HOLDS, not the one it just dropped, so
          // the host learns this seat is ahead of what it resent.
          this.sendStateAck();
          return;
        }
        this.cachedRevision = msg.revision ?? null;
        const updateSnapshot = this.cacheSnapshot(msg.state, legalActionsFromWire(msg));
        this.sendStateAck();
        this.settleWithheld();
        const settled = this.settlePendingSubmission({
          kind: "resolve",
          result: { events: msg.events, log_entries: msg.logEntries },
        });
        if (!settled) {
          this.emit({
            type: "stateChanged",
            snapshot: updateSnapshot,
            events: msg.events,
            logEntries: msg.logEntries,
          });
        }
        break;
      }
      case "state_withheld": {
        // A stale notice cannot be attributed to the parked submission: answer it like a stale `state_update`.
        if (this.cachedRevision !== null && msg.revision < this.cachedRevision) {
          this.sendStateAck();
          return;
        }
        this.cachedRevision = msg.revision;
        this.sendStateAck();
        this.withheldState = { revision: msg.revision, reason: i18n.t("multiplayer:p2pStateWithheld") };
        this.emit({ type: "stateDelivery", withheld: this.withheldState });
        // Withheld means applied: the parked action committed.
        this.settlePendingSubmission({ kind: "resolve", result: { events: [], log_entries: [] } });
        break;
      }
      case "action_rejected": {
        const error = isActionRejection(msg.rejection)
          ? actionRejectionError(msg.rejection)
          : new AdapterError(
            AdapterErrorCode.ACTION_REJECTED,
            "Host sent an invalid action rejection",
            true,
          );
        if (!this.settlePendingSubmission({ kind: "reject", error })) {
          this.emit({ type: "operationFailed", message: error.message });
        }
        break;
      }
      case "action_failed": {
        const failure = new AdapterError("P2P_ERROR", msg.message, true);
        if (!this.settlePendingSubmission({ kind: "reject", error: failure })) {
          this.emit({ type: "operationFailed", message: msg.message });
        }
        break;
      }
      case "action_noop": {
        this.settlePendingSubmission({
          kind: "resolve",
          result: { events: [], log_entries: [] },
        });
        break;
      }
      case "mana_payment_preview": {
        const pending = this.pendingManaPaymentPreviews.get(msg.requestId);
        if (pending) {
          this.pendingManaPaymentPreviews.delete(msg.requestId);
          pending.resolve(msg.sourceIds);
        }
        break;
      }
      case "mana_payment_preview_rejected": {
        const pending = this.pendingManaPaymentPreviews.get(msg.requestId);
        if (pending) {
          this.pendingManaPaymentPreviews.delete(msg.requestId);
          pending.reject(
            isActionRejection(msg.rejection)
              ? actionRejectionError(msg.rejection)
              : new AdapterError(
                AdapterErrorCode.ACTION_REJECTED,
                "Host sent an invalid mana-payment preview rejection",
                true,
              ),
          );
        }
        break;
      }
      case "mana_payment_preview_failed": {
        const pending = this.pendingManaPaymentPreviews.get(msg.requestId);
        if (pending) {
          this.pendingManaPaymentPreviews.delete(msg.requestId);
          pending.reject(new AdapterError("P2P_ERROR", msg.message, true));
        }
        break;
      }
      case "interaction_preview": {
        const pending = this.pendingInteractionPreviews.get(msg.requestId);
        // A requestId this adapter never sent settles nothing and disturbs no
        // other entry.
        if (pending) {
          this.pendingInteractionPreviews.delete(msg.requestId);
          switch (msg.answer.type) {
            case "preview":
              pending.resolve(msg.answer.preview);
              break;
            case "failed":
              pending.reject(new AdapterError("P2P_ERROR", msg.answer.message, true));
              break;
          }
        }
        break;
      }
      case "player_disconnected": {
        this.emit({
          type: "opponentDisconnected",
          reason: `Player ${msg.playerId + 1} disconnected`,
        });
        break;
      }
      case "player_reconnected": {
        this.emit({ type: "playerReconnected", playerId: msg.playerId });
        break;
      }
      case "player_kicked": {
        this.emit({
          type: "playerKicked",
          playerId: msg.playerId,
          reason: msg.reason,
        });
        break;
      }
      case "player_conceded": {
        this.emit({
          type: "playerConceded",
          playerId: msg.playerId,
          reason: msg.reason,
        });
        break;
      }
      case "game_paused": {
        this.emit({ type: "gamePaused", reason: msg.reason });
        break;
      }
      case "game_resumed": {
        this.emit({ type: "gameResumed" });
        break;
      }
      case "lobby_progress": {
        this.emit({
          type: "lobbyProgress",
          joined: msg.joined,
          total: msg.total,
        });
        break;
      }
      case "seat_snapshot": {
        this.emit({
          type: "playerSlotsUpdated",
          slots: playerSlotsFromSeatView(msg.view),
        });
        break;
      }
      case "seat_mutate": {
        break;
      }
      default:
        break;
    }
  }

  /**
   * Resolve `initializeGame()` exactly once. Called from both `game_setup`
   * (fresh join) and `reconnect_ack` (rejoining mid-game) paths; later
   * messages are ignored so the promise stays stable if the guest briefly
   * disconnects again after `initializeGame()` returns.
   */
  private settleGameSetup(result: SubmitResult): void {
    if (this.gameSetupSettled) return;
    this.gameSetupSettled = true;
    this.gameSetupResolve(result);
  }

  private rejectGameSetup(reason: string): void {
    if (this.gameSetupSettled) return;
    this.gameSetupSettled = true;
    this.gameSetupReject(new AdapterError("P2P_REJECTED", reason, false));
  }

  private rejectPendingManaPaymentPreviews(error: Error): void {
    for (const { reject } of this.pendingManaPaymentPreviews.values()) {
      reject(error);
    }
    this.pendingManaPaymentPreviews.clear();
  }

  private rejectPendingInteractionPreviews(error: Error): void {
    for (const { reject } of this.pendingInteractionPreviews.values()) {
      reject(error);
    }
    this.pendingInteractionPreviews.clear();
  }

  /**
   * The SINGLE settlement path for the parked submission slot. Every site that
   * finishes a guest submission routes through here — the four inbound reply
   * frames (`state_update`, `action_rejected`, `action_failed`, `action_noop`),
   * `rejectPendingSubmission` (attach/disconnect/terminate), the submission
   * timeout, and a displacement — so the two slot handles and the timeout are
   * cleared together, in one place.
   *
   * A settlement path that bypassed this would be a NEW bug, not a missing
   * tidy-up: the timer would survive a SUCCESSFUL submit and, because the slot
   * is unkeyed, fire `SUBMISSION_TIMEOUT_MS` later against whatever unrelated
   * submission happened to be parked by then.
   *
   * Returns whether a submission was actually parked, so callers can fall back
   * to their unsolicited-frame behaviour (`stateChanged` / `error`).
   */
  private settlePendingSubmission(outcome: PendingSubmissionOutcome): boolean {
    if (this.pendingSubmissionTimer !== null) {
      clearTimeout(this.pendingSubmissionTimer);
      this.pendingSubmissionTimer = null;
    }
    const resolve = this.pendingResolve;
    const reject = this.pendingReject;
    this.pendingResolve = null;
    this.pendingReject = null;
    if (outcome.kind === "resolve") {
      if (!resolve) return false;
      resolve(outcome.result);
      return true;
    }
    if (!reject) return false;
    reject(outcome.error);
    return true;
  }

  /**
   * Take the single submission slot for a new caller and arm its timeout.
   *
   * A submission parked while one is already pending SETTLES the displaced one
   * (retryable rejection) instead of dropping the reference and leaving that
   * caller's promise parked forever. Displacement is reachable:
   * `dispatchInteraction` never touches `isAnimating`/`inFlightLocalAction`
   * while `dispatchActionInternal` does, so an interaction submit can overlap
   * an action submit, and two concurrent `dispatchInteraction` calls can
   * displace each other.
   *
   * The slot deliberately stays single and unkeyed, and it still MIS-ROUTES
   * after a displacement: whichever reply arrives first settles the SECOND
   * submission, because no reply frame carries anything to correlate against.
   * That is pre-existing and is NOT fixed here — a correct fix needs a
   * wire-level correlation id, which means a `WIRE_PROTOCOL_VERSION` bump
   * (currently 44) and is deferred. Nothing here makes the slot correct; it
   * only stops the displaced caller from waiting on a promise that can never
   * settle. A keyed map would not help: it cannot route replies that carry no
   * id.
   */
  private parkPendingSubmission(
    resolve: (result: SubmitResult) => void,
    reject: (error: Error) => void,
  ): void {
    this.settlePendingSubmission({
      kind: "reject",
      error: new AdapterError(
        "P2P_ERROR",
        "Superseded by a later submission before the host replied",
        true,
      ),
    });
    this.pendingResolve = resolve;
    this.pendingReject = reject;
    this.pendingSubmissionTimer = setTimeout(() => {
      this.pendingSubmissionTimer = null;
      this.settlePendingSubmission({
        kind: "reject",
        error: new AdapterError(
          "P2P_ERROR",
          "The host did not answer this action in time",
          true,
        ),
      });
    }, SUBMISSION_TIMEOUT_MS);
  }

  private rejectPendingSubmission(error: Error): void {
    this.settlePendingSubmission({ kind: "reject", error });
  }

  private acceptsHostAuthority(msg: P2PMessage): boolean {
    if (!msg.authority) {
      // The lease stamp remains additive: it fences resumed hosts where it is
      // present without making an already-open room unable to settle a safe
      // terminal control frame. Version v28, unlike authority, is mandatory
      // on first contact because it changes the rejection payload shape.
      return true;
    }
    if (this.authority === null) return true;
    if (msg.type === "reconnect_ack") {
      // A legitimate same-key resume intentionally has a new incarnation.
      if (this.authority && msg.authority.sessionKey !== this.authority.sessionKey) {
        this.terminate();
        this.rejectGameSetup("Host changed the P2P session key");
        return false;
      }
      return true;
    }
    if (msg.type === "game_setup") {
      return hasExactP2PAuthority(msg.authority, this.authority);
    }
    return hasExactP2PAuthority(msg.authority, this.authority);
  }

  private send(message: P2PMessage): Promise<boolean> {
    if (!this.session) return Promise.resolve(false);
    return this.session.send({ ...message, ...(this.authority ? { authority: this.authority } : {}) });
  }

  private currentAuthenticatedSession(): PeerSession | null {
    if (
      this.terminated
      || this.session === null
      || this.authenticatedSession !== this.session
      || this.assignedPlayerId === null
    ) {
      return null;
    }
    return this.session;
  }

  private requireAuthenticatedSession(): PeerSession {
    const session = this.currentAuthenticatedSession();
    if (session) return session;
    if (this.session === null) {
      throw new AdapterError("P2P_ERROR", "Not connected to host", true);
    }
    throw new AdapterError("P2P_ERROR", "Not yet assigned a player ID", true);
  }

  private handleHostDisconnect(session: PeerSession): void {
    if (session !== this.session) return;
    this.rejectPendingSubmission(
      new AdapterError("P2P_ERROR", "Host disconnected while submitting an action", true),
    );
    this.rejectPendingManaPaymentPreviews(
      new AdapterError("P2P_ERROR", "Host disconnected during mana-payment preview", true),
    );
    this.rejectPendingInteractionPreviews(
      new AdapterError("P2P_ERROR", "Host disconnected during interaction preview", true),
    );
    this.authenticatedSession = null;
    this.matchConcedeSent = false;
    this.session = null;
    if (!this.playerToken && !this.gameSetupSettled) {
      // A fresh guest has no token with which a new connection could identify
      // itself. Retrying the transport would reopen an unauthenticated channel
      // that sends nothing, leaving initializeGame() pending forever.
      const reason = i18n.t("multiplayer:reconnectRejected.hostDisconnectedBeforeSetup");
      this.terminate();
      this.rejectGameSetup(reason);
      this.emit({ type: "reconnectFailed", reason });
      return;
    }
    // Suppress auto-reconnect in terminal states (kicked, explicitly rejected,
    // or adapter disposed). Without this, a kicked guest would spin the
    // backoff schedule (~30s total) hammering the host with a blacklisted
    // token.
    if (this.terminated) return;
    void this.attemptReconnect(0);
  }

  private terminate(error = new AdapterError("P2P_ERROR", "Host session terminated", true)): void {
    this.terminated = true;
    this.authenticatedSession = null;
    this.rejectPendingSubmission(error);
    this.rejectPendingManaPaymentPreviews(error);
    this.rejectPendingInteractionPreviews(error);
    const session = this.session;
    this.session = null;
    session?.close();
  }

  private async attemptReconnect(attemptIndex: number): Promise<void> {
    if (this.terminated) return;
    // After the escalating schedule, retry at a steady 60s cadence until
    // the user explicitly leaves. This is the "host-is-taking-a-while-
    // to-come-back" case (browser crash + reopen + tab-warmup can easily
    // take 2-3 minutes). `reconnectFailed` is NOT emitted here — the UI
    // keeps the reconnecting indicator up and the user decides when to
    // give up.
    const delay = attemptIndex < RECONNECT_BACKOFF_MS.length
      ? RECONNECT_BACKOFF_MS[attemptIndex]
      : RECONNECT_STEADY_STATE_MS;
    this.emit({ type: "reconnecting", attempt: attemptIndex + 1 });
    await new Promise((r) => setTimeout(r, delay));
    if (this.terminated) return;

    try {
      // `PEER_CONNECT_OPTIONS` is not optional: without `reliable: true` this
      // reconnect channel comes up UNORDERED, and every revision guard
      // downstream assumes ordered delivery. The initial `joinRoom` dial has
      // always carried them; this one did not.
      const conn = dialPeer(this.hostPeer, this.hostPeerId, RECONNECT_DIAL_TIMEOUT_MS);
      await new Promise<void>((resolve, reject) => {
        const timeout = setTimeout(
          () => reject(new Error("connect timed out")),
          RECONNECT_DIAL_TIMEOUT_MS,
        );
        conn.on("open", () => {
          clearTimeout(timeout);
          resolve();
        });
        conn.on("error", (err) => {
          clearTimeout(timeout);
          reject(err);
        });
      });
      if (this.terminated) {
        conn.close();
        return;
      }
      this.attachSession(conn);
      if (this.terminated) return;
      if (this.playerToken) {
        this.send({
          type: "reconnect",
          playerToken: this.playerToken,
          wireProtocolVersion: WIRE_PROTOCOL_VERSION,
          ...(this.authority ? { sessionKey: this.authority.sessionKey } : {}),
        });
      }
    } catch (err) {
      if (this.terminated) return;
      console.warn(
        `[P2PGuest] reconnect attempt ${attemptIndex + 1} failed:`,
        err,
      );
      void this.attemptReconnect(attemptIndex + 1);
    }
  }
}
