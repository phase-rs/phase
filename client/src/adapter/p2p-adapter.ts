import type Peer from "peerjs";
import type { DataConnection } from "peerjs";

import type {
  AiActionProposal,
  AiDecisionDiagnosticReceipt,
  AiProposalSubmission,
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
  SubmitResult,
  WaitingFor,
} from "./types";
import type { InteractionSubmission } from "./generated/interaction";
import type { BracketDeckRequest, BracketEstimate } from "../types/bracketEstimate";

import { AdapterError, AdapterErrorCode, EMPTY_LEGAL_ACTIONS, actionRejectionError, nextSnapshotSeq } from "./types";
import { getHostAdapter } from "./wasm-adapter";
import {
  WebSocketAdapter,
  type NativeAiSeat,
  type NativeSessionAttachment,
} from "./ws-adapter";
import { createPeerSession, type PeerSession } from "../network/peer";
import type { P2PMessage } from "../network/protocol";
import { WIRE_PROTOCOL_VERSION, legalActionsFromWire, legalActionsToWire } from "../network/protocol";
import type {
  PlayerSlot,
  SeatKind,
  SeatMutation,
  SeatState,
  SeatMutationResult,
  SeatView,
} from "../multiplayer/seatTypes";
import type { BrokerClient } from "../services/brokerClient";
import type { FullSessionKey } from "../services/multiplayerSession";
import {
  clearP2PHostSession,
  type NativeAiDriverFault,
  type NativeP2PServerSession,
  type PersistedP2PHostSession,
  saveGame,
  saveP2PHostSession,
} from "../services/gamePersistence";
import {
  claimP2PHostLease,
  createP2PSessionKey,
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
  type P2PTerminalResult,
} from "../services/p2pTerminalResult";
import { NativeEngineSocket } from "../services/nativeEngineSocket";

/**
 * Adapter-level events emitted to the UI. Wire-protocol messages are
 * snake_case (`player_kicked`); adapter events stay camelCase
 * (`playerKicked`). The adapter performs the remap inside its message
 * handlers — the UI never sees wire types.
 */
export type P2PAdapterEvent =
  | { type: "playerIdentity"; playerId: PlayerId; playerNames?: Record<number, string> }
  | { type: "roomCreated"; roomCode: string }
  | { type: "waitingForGuest" }
  | { type: "guestConnected" }
  | { type: "opponentDisconnected"; reason: string }
  | { type: "gameOver"; winner: PlayerId | null; reason: string }
  | { type: "terminalResult"; result: P2PTerminalResult }
  | { type: "terminalUnavailable"; message: string }
  | { type: "error"; message: string }
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
  | { type: "reconnectFailed"; reason: string };

type P2PAdapterEventListener = (event: P2PAdapterEvent) => void;

interface DeckSeatPayload {
  main_deck: string[];
  sideboard: string[];
  commander: string[];
  companion?: string[];
  signature_spell?: string[];
  planar_deck?: string[];
  scheme_deck?: string[];
  bracket_tier?: string;
}

interface DeckListPayload {
  player: DeckSeatPayload;
  opponent: DeckSeatPayload;
  ai_decks: DeckSeatPayload[];
  /** AI difficulty strings per seat. See `DeckList.ai_difficulties` in engine. */
  ai_difficulties?: string[];
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
};

/**
 * Local-only multiplexor for a native authoritative P2P host. There is one
 * loopback socket for every human P2P seat; the server authenticates each
 * action from that socket while PeerJS remains the guest-facing transport.
 */
class NativeP2PBridge {
  private readonly clients = new Map<PlayerId, WebSocketAdapter>();
  private readonly playerTokens = new Map<PlayerId, string>();
  private readonly latestViews = new Map<PlayerId, NativeViewerUpdate>();
  private readonly pendingViews = new Map<number, Map<PlayerId, NativeViewerUpdate>>();
  private readonly startWaiters: Array<(update: NativeViewerUpdate) => void> = [];
  /** Preserve the server's revision order while asynchronous PeerJS frame
   * encoding runs; a terminal commitment must follow its final state frame. */
  private revisionQueue: Promise<void> = Promise.resolve();
  private deliveredRevision = -1;
  private deliveredFaultIds = new Set<number>();
  private readonly pendingFaults = new Map<number, { id: number; revision: number; message: string }>();
  private gameCode: string | null = null;
  private fullKey: FullSessionKey | null = null;

  constructor(
    private readonly hostDeck: DeckListPayload["player"],
    private readonly hostDisplayName: string,
    private readonly playerCount: number,
    private readonly formatConfig: FormatConfig | undefined,
    private readonly matchConfig: MatchConfig | undefined,
    private readonly options: NativeP2PHostOptions,
    private readonly onRevision: (revision: number, views: Map<PlayerId, NativeViewerUpdate>) => Promise<void>,
    private readonly onFault: (fault: { id: number; revision: number; message: string }) => Promise<void>,
    private readonly resumeSession?: NativeP2PServerSession,
  ) {}

  async initializeHost(aiSeats: NativeAiSeat[]): Promise<NativeSessionAttachment> {
    if (this.resumeSession) {
      this.gameCode = this.resumeSession.gameCode;
      this.fullKey = this.resumeSession.fullKey;
      const hostToken = this.resumeSession.playerTokens[0];
      if (!hostToken) {
        throw new AdapterError("P2P_ERROR", "Native resume is missing the host token", false);
      }
      const hostAttachment = await this.reconnectClient(0, hostToken);
      for (const [pidText, token] of Object.entries(this.resumeSession.playerTokens)) {
        const playerId = Number(pidText);
        if (playerId === 0) continue;
        await this.reconnectClient(playerId, token);
      }
      return hostAttachment;
    }
    const host = new WebSocketAdapter(
      "native-engine://phase-server",
      "host",
      this.hostDeck,
      undefined,
      undefined,
      undefined,
      this.hostDisplayName,
      {
        nativePregame: {
          kind: "host",
          socketFactory: () => new NativeEngineSocket(),
          expectedServerVersion: this.options.expectedServerVersion,
          playerCount: this.playerCount,
          aiSeats,
          formatConfig: this.formatConfig,
          matchConfig: this.matchConfig,
        },
      },
    );
    const initialSlots = host.waitForPlayerSlots();
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
        },
      },
    );
    const hostSlots = this.clientFor(0).waitForPlayerSlots();
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
    await host.sendSeatMutation({ type: "Start" });
    await Promise.all(waits);
    const hostUpdate = await started;
    return { events: hostUpdate.events, log_entries: hostUpdate.logEntries };
  }

  async applySeatMutation(mutation: SeatMutation): Promise<void> {
    await this.clientFor(0).sendSeatMutation(mutation);
  }

  async submitAction(action: GameAction, playerId: PlayerId): Promise<SubmitResult> {
    return this.clientFor(playerId).submitAction(action, playerId);
  }

  async submitInteraction(
    submission: InteractionSubmission,
    playerId: PlayerId,
  ): Promise<SubmitResult> {
    return this.clientFor(playerId).submitInteraction(submission, playerId);
  }

  async previewManaPayment(action: GameAction, playerId: PlayerId): Promise<ObjectId[]> {
    return this.clientFor(playerId).previewManaPayment(action, playerId);
  }

  async getState(): Promise<GameState> {
    return this.clientFor(0).getState();
  }

  async getLegalActions(): Promise<LegalActionsResult> {
    return this.clientFor(0).getLegalActions();
  }

  async getSnapshot(): Promise<EngineSnapshot> {
    return this.clientFor(0).getSnapshot();
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

  detachGuest(playerId: PlayerId): void {
    if (playerId === 0) return;
    this.clients.get(playerId)?.dispose();
    this.clients.delete(playerId);
    this.playerTokens.delete(playerId);
    this.latestViews.delete(playerId);
  }

  dispose(): void {
    for (const client of this.clients.values()) client.dispose();
    this.clients.clear();
    this.playerTokens.clear();
    this.latestViews.clear();
    this.pendingViews.clear();
  }

  private async attachClient(client: WebSocketAdapter): Promise<NativeSessionAttachment> {
    client.onEvent((event) => {
      if (event.type === "sessionAttached") {
        // Register the exact authenticated seat before GameStarted/reconnect
        // can release a local state frame. This membership is the barrier's
        // recipient set for every following revision.
        this.clients.set(event.attachment.playerId, client);
        this.playerTokens.set(event.attachment.playerId, event.attachment.playerToken);
        return;
      }
      if (event.type === "aiDriverFault") {
        if (this.deliveredFaultIds.has(event.id)) return;
        this.pendingFaults.set(event.id, { id: event.id, revision: event.revision, message: event.message });
        this.enqueueFaultBarrier();
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
      };
      this.latestViews.set(playerId, update);
      const views = this.pendingViews.get(revision) ?? new Map<PlayerId, NativeViewerUpdate>();
      views.set(playerId, update);
      this.pendingViews.set(revision, views);
      if (views.size !== this.clients.size) return;
      this.pendingViews.delete(revision);
      this.revisionQueue = this.revisionQueue
        .then(async () => {
          await this.onRevision(revision, views);
          this.deliveredRevision = Math.max(this.deliveredRevision, revision);
          await this.releaseFaultsThroughDeliveredRevision();
        })
        .catch((error) => {
          console.error("[NativeP2PBridge] revision fan-out failed:", error);
        });
      const hostUpdate = views.get(0);
      if (hostUpdate) {
        for (const resolve of this.startWaiters.splice(0)) resolve(hostUpdate);
      }
    });
    const attachment = await client.initializePregame();
    this.clients.set(attachment.playerId, client);
    this.playerTokens.set(attachment.playerId, attachment.playerToken);
    return attachment;
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

  persistence(): NativeP2PServerSession | null {
    if (!this.gameCode || !this.fullKey) return null;
    return {
      gameCode: this.gameCode,
      fullKey: this.fullKey,
      playerTokens: Object.fromEntries(this.playerTokens),
    };
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
          gameCode: this.gameCode,
          playerId,
          playerToken,
          fullKey: this.fullKey,
        },
      },
    );
    const attachment = await this.attachClient(client);
    if (attachment.playerId !== playerId) {
      client.dispose();
      throw new AdapterError("P2P_ERROR", "Native reconnect returned the wrong player seat", false);
    }
    return attachment;
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
// A stale proposal leaves the prompt unchanged, so cap retries to prevent a
// persistent authority race from becoming a tight host-loop spin.
const MAX_AI_PROPOSAL_STALE_RETRIES = 3;

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
      deck_size: 60,
      singleton: false,
      command_zone: false,
      commander_damage_threshold: null,
      range_of_influence: null,
      team_based: false,
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
  private nativeBridge: NativeP2PBridge | null = null;
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
  private gameRunState: GameRunState = "running";
  /** Monotonic authority revision for WASM hosts; native hosts replace this
   * with the local phase-server's revision before fan-out. */
  private authoritativeRevision = 0;
  /** First committed terminal statement fences every subsequent action and
   * reconnect. Its id is immutable for this adapter incarnation. */
  private terminalResult: P2PTerminalResult | null = null;
  /** Native AI faults are terminal and must also be replayed to a guest that
   * reconnects after the live PeerJS fan-out completed. */
  private nativeAiDriverFault: NativeAiDriverFault | null = null;
  /** The local host must render a restored native fault once the bridge has
   * delivered its fenced final snapshot. Keep this separate from the durable
   * fault itself: rehydration sets the latter before the bridge replays it. */
  private deliveredNativeAiDriverFault: NativeAiDriverFault | null = null;
  readonly supportsMatchConcede: true | undefined;
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

  constructor(
    private readonly hostDeckData: unknown,
    private readonly hostPeer: Peer,
    /**
     * Subscribe to inbound guest `DataConnection`s via `hostRoom()`'s
     * documented API. Using this (instead of `hostPeer.on("connection")`
     * directly) avoids double-dispatch with `hostRoom()`'s internal
     * listener, and drains any connections that were buffered while the
     * adapter was still under construction.
     */
    private readonly onGuestConnected: (
      handler: (conn: DataConnection) => void,
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
      resumeData?: { state?: PersistedGameState; session: PersistedP2PHostSession };
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
      this.rehydrateFromPersistedSession(persistence.resumeData.session);
      this.pregameSeatView = seatStateToView(this.pregameSeatState);
    }
    const nativeResume = persistence?.resumeData?.session.nativeSession;
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
        (hostDeckData as DeckListPayload).player,
        this.hostDisplayName ?? "Host",
        playerCount,
        formatConfig,
        matchConfig,
        native,
        (revision, views) => this.handleNativeRevision(revision, views),
        (fault) => this.handleNativeAiDriverFault(fault),
        nativeResume,
      );
    } else {
      this.attachBrowserAiDecisionDiagnostics();
    }
  }

  /**
   * Installs the local-only diagnostics capability once this host is backed by
   * browser WASM. It deliberately remains an instance property: native hosts
   * and P2P guests must fail capability detection rather than receive a no-op.
   */
  private attachBrowserAiDecisionDiagnostics(): void {
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
    return {
      gameId: this.gameId,
      roomCode: this.roomCode,
      sessionKey: this.sessionKey,
      brokerGameCode: this.brokerGameCode,
      useBroker: this.broker !== undefined,
      playerTokens,
      guestDecks,
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
    const snapshot = this.buildPersistedSession();
    if (!snapshot) return;
    void saveP2PHostSession(this.gameId, snapshot);
  }

  /** Persist the host authority as the engine's opaque trusted envelope. */
  private persistAuthoritativeState(): void {
    if (!this.ownsAuthority()) return;
    if (this.nativeBridge) return;
    if (!this.gameId) return;
    void this.wasm
      .exportPersistenceState()
      .then((json) => saveGame(this.gameId!, JSON.parse(json) as PersistedGameState))
      .catch((err) => {
        console.warn("[P2PHost] trusted state export failed:", err);
      });
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
  private async bailDisposed(claimed: boolean, during: string): Promise<never> {
    await this.wasm.releaseHostSession(claimed);
    throw new AdapterError("P2P_ERROR", `Host session disposed during ${during}`, true);
  }

  private send(session: PeerSession, message: P2PMessage): Promise<void> {
    if (!this.ownsAuthority()) return Promise.resolve();
    return session.send({ ...message, authority: this.authority });
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
        throw new AdapterError("P2P_ERROR", `AI proposal rejected: ${outcome.reason}`, false);
      }
      staleRetries = 0;
      const result = outcome.result;
      await this.broadcastStateUpdate(result.events, result.log_entries);
      this.persistAuthoritativeState();
      this.emit({
        type: "stateChanged",
        snapshot: await this.wasm.getSnapshot(),
        events: result.events,
        logEntries: result.log_entries,
      });
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
          this.attachBrowserAiDecisionDiagnostics();
        }
      }
      // A teardown may have landed while `wasm.initialize()` (and the native
      // handshake above) were in flight. Bail before installing anything:
      // nothing has been claimed yet, so there is nothing to undo.
      if (this.disposed) await this.bailDisposed(false, "initialization");
      // Resume path: load the persisted GameState with a fresh RNG seed
      // and atomic multiplayer-flag claim. `resumeMultiplayerHostState`
      // mirrors server-core's `from_persisted` pattern, and
      // `initializeMultiplayerHostGame` is its fresh-start sibling — both
      // refuse an engine that is already in use and claim the flag themselves
      // in the same call that installs the state. No client code sets the flag,
      // so an open lobby leaves zero engine footprint.
      if (this.isResume && this.resumeGameState) {
        await this.wasm.resumeMultiplayerHostState(this.resumeGameState);
        this.resumeGameState = null;
        // The engine now holds both this game's state and the multiplayer
        // flag. Its await window is the widest in the adapter (the full card
        // DB load happens inside), so re-check before recording the claim.
        if (this.disposed) await this.bailDisposed(true, "resume");
        sharedEngineHost = this.engineClaim;
        traceAdapter("Host", "initialize-resume", {
          tokens: this.playerTokens.size,
          gameStarted: this.gameStarted,
        });
      }
      this.resolvePregameReady();
    } catch (err) {
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
    }
    traceAdapter("Host", "initialize-complete", {});
    this.initialized = true;
  }

  private handleNewConnection(conn: DataConnection): void {
    if (!this.ownsAuthority()) {
      const session = createPeerSession(conn, {});
      this.rejectSuperseded(session);
      return;
    }
    traceAdapter("Host", "handle-new-connection", { connOpen: conn.open });
    // Reconnect path: the first message determines whether this is a fresh
    // join or a reconnect. We attach a one-shot pre-handler to peek at the
    // first message before wrapping in a PeerSession with full handlers.
    const session = createPeerSession(conn, {
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
      },
    });

    let identified = false;
    const unsub = session.onMessage((msg) => {
      if (identified) return;
      identified = true;
      unsub();

      // Host-side wire-version gate. A guest that stamps a version unequal
      // to ours cannot be paired, so refuse it here — before a seat or a deck
      // is allocated — through the rejection affordance this gate already
      // has. `reconnect_rejected` is `{ type, reason }` only, so even a guest
      // on an older bundle can decode it.
      //
      // ABSENT MEANS ADMIT. The version is bumped only on a
      // non-backward-compatible change, so {guests on an older bundle}
      // strictly contains {guests that are wire-incompatible}: an absent
      // field reports bundle age, not incompatibility, and refusing on it
      // would drop guests that pair fine today with a reason that is
      // factually false for them. The consequence, stated rather than
      // implied: this host cannot distinguish a compatible old bundle from
      // an incompatible one. `P2PGuestAdapter.handleHostMessage` is the
      // authority for the latter. This mirrors the call `acceptsHostAuthority`
      // already makes for its own additive stamp. What this gate buys is
      // prospective: once both sides ship, every future bump is caught
      // host-side at first contact.
      const guestVersion =
        msg.type === "guest_deck" || msg.type === "reconnect" ? msg.wireProtocolVersion : undefined;
      if (guestVersion !== undefined && guestVersion !== WIRE_PROTOCOL_VERSION) {
        // Reaches the refused guest's user RAW, the same way the guest-side
        // reason at `handleHostMessage` does. `i18n/README.md` asks for `t()`
        // on frontend-authored strings; this one is new text on that raw path
        // and is written unkeyed deliberately, to match the incumbent
        // wording rather than split the pair. Noted, not fixed: the raw-string
        // path is pre-existing and out of scope here.
        const reason = `Wire protocol mismatch: guest sent v${guestVersion}, this host speaks v${WIRE_PROTOCOL_VERSION}. Refresh both windows.`;
        traceAdapter("Host", "first-message-version-mismatch", { type: msg.type, guestVersion });
        void this.send(session, { type: "reconnect_rejected", reason });
        session.close("Wire protocol mismatch");
        return;
      }

      if (msg.type === "reconnect") {
        traceAdapter("Host", "first-message", { type: msg.type });
        this.handleReconnect(session, msg.playerToken, msg.sessionKey);
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
      } else {
        traceAdapter("Host", "first-message", { type: msg.type });
        // Unexpected first message — reject.
        void this.send(session, {
          type: "reconnect_rejected",
          reason: "Expected guest_deck or reconnect as first message",
        });
        session.close("Protocol violation");
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
          this.attachBrowserAiDecisionDiagnostics();
        }
      }
      if (!this.ownsAuthority()) {
        this.rejectSuperseded(session);
        return;
      }

      const token = crypto.randomUUID();
      this.playerTokens.set(pid, token);
      this.guestSessions.set(pid, session);
      this.guestDecks.set(pid, guestDeck);
      if (displayName) this.guestNames.set(pid, displayName);
      this.pregameSeatState.seats[pid] = { type: "JoinedHuman" };
      this.pregameSeatState.tokens[pid] = token;
      await this.refreshPregameSeatView();
      this.saveSession();

      session.onMessage((msg) => this.handleGuestMessage(pid, msg));

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
        // Validate against the worker engine's already-resident card DB
        // (`checkDeckCompatibility` self-ensures it). Using the main-thread
        // `engineRuntime` instance instead would parse a SECOND full ~93 MB
        // card DB into the page — doubling host footprint past iOS Safari's
        // per-tab memory ceiling and silently OOM-reloading the host tab.
        const result = await this.wasm.checkDeckCompatibility({
          main_deck: deck.main_deck,
          sideboard: deck.sideboard,
          commander: deck.commander ?? [],
          companion: deck.companion ?? [],
          signature_spell: deck.signature_spell ?? [],
          selected_format: this.formatConfig!.format,
        }) as { selected_format_compatible?: boolean | null; selected_format_reasons: string[] };

        if (!this.ownsAuthority()) return;
        if (this.gameStarted) return;
        if (result.selected_format_compatible === false) {
          const reason = result.selected_format_reasons[0]
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
      this.attachBrowserAiDecisionDiagnostics();
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
      let result: SubmitResult;
      try {
        result = await this.wasm.initializeMultiplayerHostGame(
          deckPayload,
          this.formatConfig,
          playerCount,
          this.matchConfig,
          undefined,
        );
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
        await this.wasm.releaseHostSession(false);
        throw err;
      }
      // The engine now holds this game. Record the claim only now: claiming
      // before the engine accepted would let a refused call's teardown clear
      // state this session never owned.
      if (this.disposed) await this.bailDisposed(true, "start");
      // Checked before the stamp: a host whose lease was superseded mid-start
      // must not take the claim from the host that superseded it. It did
      // install engine state, so it hands that state back rather than leaving
      // it for someone else's teardown to find unclaimed.
      if (!this.ownsAuthority()) {
        await this.wasm.releaseHostSession(true);
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
      for (const [pid, session] of this.guestSessions) {
        const token = this.playerTokens.get(pid)!;
        const snapshot = await this.wasm.getViewerSnapshot(pid);
        void this.send(session, {
          type: "game_setup",
          wireProtocolVersion: WIRE_PROTOCOL_VERSION,
          assignedPlayerId: pid,
          playerToken: token,
          revision,
          state: snapshot.state,
          events: result.events,
          playerNames: allNames,
          ...legalActionsToWire(snapshot),
        });
      }

      await this.runAiLoop();
      return result;
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
    if (this.gameRunState !== "running") {
      throw new AdapterError(
        "P2P_PAUSED",
        `Cannot submit action while game state is ${this.gameRunState}`,
        true,
      );
    }
    const result = this.nativeBridge
      ? await this.nativeBridge.submitAction(action, actor)
      : await this.wasm.submitAction(action, actor);
    if (isZeroCountDebugCreate(action)) return result;
    await this.broadcastStateUpdate(result.events, result.log_entries);
    await this.runAiLoop();
    this.persistAuthoritativeState();
    return result;
  }

  async submitInteraction(
    submission: InteractionSubmission,
    actor: PlayerId,
  ): Promise<SubmitResult> {
    this.assertNotDisposed();
    if (!this.ownsAuthority()) {
      throw new AdapterError("P2P_ERROR", "Host session superseded", true);
    }
    if (this.gameRunState !== "running") {
      throw new AdapterError(
        "P2P_PAUSED",
        `Cannot submit interaction while game state is ${this.gameRunState}`,
        true,
      );
    }
    const result = this.nativeBridge
      ? await this.nativeBridge.submitInteraction(submission, actor)
      : await this.wasm.submitInteraction(submission, actor);
    await this.broadcastStateUpdate(result.events, result.log_entries);
    await this.runAiLoop();
    this.persistAuthoritativeState();
    return result;
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

  async exportPersistenceState(): Promise<string> {
    this.assertNotDisposed();
    if (!this.ownsAuthority()) {
      throw new AdapterError("P2P_ERROR", "Host session superseded", true);
    }
    if (this.nativeBridge) {
      throw new AdapterError("P2P_ERROR", "Native P2P persistence is managed by phase-server", false);
    }
    return this.wasm.exportPersistenceState();
  }

  /** Releases a complete native-server revision only after every local seat
   * socket has supplied its own filtered view. The native server is the
   * authority for this revision; PeerJS carries it through to terminal
   * correlation rather than inventing another clock. */
  private async handleNativeRevision(
    revision: number,
    views: Map<PlayerId, NativeViewerUpdate>,
  ): Promise<void> {
    if (!this.ownsAuthority()) return;
    const hostUpdate = views.get(0);
    if (!hostUpdate) return;
    if (revision < this.authoritativeRevision) return;
    this.authoritativeRevision = revision;
    const allNames = this.playerNamesForSeats();
    const sends: Array<Promise<void>> = [];
    for (const [pid, session] of this.guestSessions) {
      const update = views.get(pid);
      if (!update || this.disconnectedSeats.has(pid)) continue;
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
    this.emit({
      type: "stateChanged",
      snapshot: hostUpdate.snapshot,
      events: hostUpdate.events,
      logEntries: hostUpdate.logEntries,
    });
    await this.commitTerminalIfComplete(hostUpdate.snapshot, revision);
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
   */
  private async commitTerminalIfComplete(
    snapshot: EngineSnapshot,
    revision: number,
    reason: string = "Game complete",
  ): Promise<void> {
    const { waiting_for: waitingFor } = snapshot.state;
    if (waitingFor.type !== "GameOver" || this.terminalResult) return;
    const winner = waitingFor.data.winner;
    const display = { winner, reason };
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
    const result = await createResult(0, snapshot.state);
    if (!(await commitP2PTerminalResult(result))) {
      this.emit({ type: "terminalUnavailable", message: "Failed to retain P2P terminal result" });
      return;
    }
    this.terminalResult = result;
    this.gameRunState = "terminal";
    await Promise.all([...this.guestSessions].map(async ([playerId, session]) => {
      const viewerSnapshot = this.nativeBridge
        ? this.nativeBridge.viewerSnapshot(playerId)
        : await this.wasm.getViewerSnapshot(playerId);
      const recipientResult = await createResult(playerId, viewerSnapshot.state);
      await this.send(session, { type: "terminal_result", result: recipientResult });
    }));
    this.emit({ type: "terminalResult", result });
  }

  /**
   * A terminal statement is recipient-bound because its commitment covers the
   * recipient's filtered final state. Reconnects therefore need a newly
   * committed statement rather than replaying the host's retained result.
   */
  private async terminalResultForRecipient(
    recipient: PlayerId,
    terminalState: GameState,
  ): Promise<P2PTerminalResult> {
    const terminal = this.terminalResult;
    if (terminal === null) throw new Error("No terminal result to deliver");
    return {
      key: this.sessionKey,
      lease: this.authority,
      recipient,
      revision: this.authoritativeRevision,
      terminalId: crypto.randomUUID(),
      finalStateCommitment: await p2pFinalStateCommitment(terminalState),
      display: terminal.display,
    };
  }

  /**
   * Fan out a state update to every connected guest. Each guest gets its own
   * `ViewerSnapshot` via the engine's combined filter+legal-actions call (one
   * WASM round-trip per guest instead of two). Only the acting guest gets a
   * populated `legalActions` map; non-acting guests receive empty legal
   * actions from the engine-side viewer gate (`legal_actions_for_viewer`).
   * Skips disconnected seats (their state is delivered via `reconnect_ack`).
   */
  private async broadcastStateUpdate(
    events: GameEvent[],
    logEntries?: GameLogEntry[],
    terminalReason?: string,
  ): Promise<void> {
    if (!this.ownsAuthority()) return;
    if (this.nativeBridge) return;
    const revision = ++this.authoritativeRevision;
    const sends: Array<Promise<void>> = [];
    for (const [pid, session] of this.guestSessions) {
      if (this.disconnectedSeats.has(pid)) continue;
      const snapshot = await this.wasm.getViewerSnapshot(pid);
      sends.push(this.send(session, {
        type: "state_update",
        revision,
        state: snapshot.state,
        events,
        logEntries,
        ...legalActionsToWire(snapshot),
      }));
    }
    await Promise.all(sends);
    await this.commitTerminalIfComplete(await this.wasm.getSnapshot(), revision, terminalReason);
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
      await this.broadcastStateUpdate(outcome.result.events, outcome.result.log_entries);
      await this.runAiLoop();
      this.persistAuthoritativeState();
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
    await this.concedePlayer(0, "Host conceded", "conceded");
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
    for (const session of this.guestSessions.values()) {
      session.close();
    }
    this.guestSessions.clear();
    this.kickedTokens.clear();
    this.playerTokens.clear();
    this.guestDecks.clear();
    this.aiDecks.clear();
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
    if (sharedEngineHost === this.engineClaim) {
      sharedEngineHost = null;
      void this.wasm.releaseHostSession(true);
    } else {
      void this.wasm.releaseHostSession(false);
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

  private async handleGuestMessage(
    pid: PlayerId,
    msg: P2PMessage,
  ): Promise<void> {
    const session = this.guestSessions.get(pid);
    if (!this.ownsAuthority()) {
      if (session) this.rejectSuperseded(session);
      return;
    }
    if (
      msg.authority
      && (msg.authority.sessionKey !== this.authority.sessionKey
        || msg.authority.hostIncarnation !== this.authority.hostIncarnation)
    ) {
      if (session) {
        void this.send(session, { type: "action_rejected", reason: "Stale host incarnation" });
      }
      return;
    }
    switch (msg.type) {
      case "action": {
        // Verify sender identity to prevent guest 2 spoofing as guest 3.
        if (msg.senderPlayerId !== pid) {
          const session = this.guestSessions.get(pid);
          if (session) {
            void this.send(session, {
              type: "action_rejected",
              reason: `senderPlayerId mismatch (declared ${msg.senderPlayerId}, session owns ${pid})`,
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
              type: "action_rejected",
              reason: "Player has conceded and can no longer act",
            });
          }
          return;
        }
        if (this.gameRunState !== "running") {
          const session = this.guestSessions.get(pid);
          if (session) {
            void this.send(session, {
              type: "action_rejected",
              reason: `Game ${this.gameRunState}`,
            });
          }
          return;
        }
        try {
          // CRITICAL: pass `pid` (the session-bound PlayerId), NEVER
          // `msg.senderPlayerId`. The envelope check above already guarantees
          // they match, but if we ever regressed that check we must still
          // tag with the authenticated session identity — the wire payload
          // is untrusted. This is the defense-in-depth that makes the engine
          // guard meaningful for P2P.
          const result = this.nativeBridge
            ? await this.nativeBridge.submitAction(msg.action, pid)
            : await this.wasm.submitAction(msg.action, pid);
          if (isZeroCountDebugCreate(msg.action)) {
            const session = this.guestSessions.get(pid);
            if (session) await this.send(session, { type: "action_noop" });
            break;
          }
          await this.broadcastStateUpdate(result.events, result.log_entries);
          // Wake the AI loop. After a guest's action lands, priority may have
          // shifted to an AI seat — without this, the AI never gets a turn
          // and the game stalls (same pattern as concedePlayer/host submit).
          await this.runAiLoop();
          this.persistAuthoritativeState();
          // Emit local stateChanged so host UI updates for opponent actions.
          if (!this.nativeBridge) {
            this.emit({
              type: "stateChanged",
              snapshot: await this.wasm.getSnapshot(),
              events: result.events,
              logEntries: result.log_entries,
            });
          }
        } catch (err) {
          const reason = err instanceof Error ? err.message : String(err);
          const session = this.guestSessions.get(pid);
          if (session) void this.send(session, { type: "action_rejected", reason });
        }
        break;
      }
      case "interaction": {
        const session = this.guestSessions.get(pid);
        if (!session || msg.senderPlayerId !== pid) {
          if (session) void this.send(session, { type: "action_rejected", reason: "senderPlayerId mismatch" });
          return;
        }
        if (this.eliminatedSeats.has(pid) || this.gameRunState !== "running") {
          void this.send(session, {
            type: "action_rejected",
            reason: this.eliminatedSeats.has(pid)
              ? "Player has conceded and can no longer act"
              : `Game ${this.gameRunState}`,
          });
          return;
        }
        try {
          const result = this.nativeBridge
            ? await this.nativeBridge.submitInteraction(msg.submission, pid)
            : await this.wasm.submitInteraction(msg.submission, pid);
          await this.broadcastStateUpdate(result.events, result.log_entries);
          await this.runAiLoop();
          this.persistAuthoritativeState();
          if (!this.nativeBridge) {
            this.emit({
              type: "stateChanged",
              snapshot: await this.wasm.getSnapshot(),
              events: result.events,
              logEntries: result.log_entries,
            });
          }
        } catch (err) {
          void this.send(session, {
            type: "action_rejected",
            reason: err instanceof Error ? err.message : String(err),
          });
        }
        break;
      }
      case "preview_mana_payment": {
        const session = this.guestSessions.get(pid);
        if (!session) return;
        if (this.eliminatedSeats.has(pid)) {
          void this.send(session, {
            type: "mana_payment_preview_rejected",
            requestId: msg.requestId,
            reason: "Player has conceded and can no longer act",
          });
          return;
        }
        if (this.gameRunState !== "running") {
          void this.send(session, {
            type: "mana_payment_preview_rejected",
            requestId: msg.requestId,
            reason: `Game ${this.gameRunState}`,
          });
          return;
        }
        try {
          const sourceIds = this.nativeBridge
            ? await this.nativeBridge.previewManaPayment(msg.action, pid)
            : await this.wasm.previewManaPayment(msg.action, pid);
          void this.send(session, { type: "mana_payment_preview", requestId: msg.requestId, sourceIds });
        } catch (err) {
          const reason = err instanceof Error ? err.message : String(err);
          void this.send(session, {
            type: "mana_payment_preview_rejected",
            requestId: msg.requestId,
            reason,
          });
        }
        break;
      }
      case "concede": {
        // CR 104.3a: Any player may concede at any time. Route through the
        // engine action so the seat is properly eliminated (CR 800.4a).
        await this.concedePlayer(pid, "Player conceded", "conceded");
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
              type: "action_rejected",
              reason: "Whole-match concession is unavailable for this game",
            });
          }
          return;
        }
        this.requestBoundMatchConcede(pid);
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

  private handleReconnect(
    session: PeerSession,
    playerToken: string,
    sessionKey?: P2PSessionKey,
  ): void {
    if (!this.ownsAuthority()) {
      this.rejectSuperseded(session);
      return;
    }
    if (sessionKey !== undefined && sessionKey !== this.sessionKey) {
      void this.send(session, { type: "reconnect_rejected", reason: "Wrong P2P session" });
      session.close("Wrong P2P session");
      return;
    }
    if (this.kickedTokens.has(playerToken)) {
      void this.send(session, { type: "reconnect_rejected", reason: "Player kicked" });
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
      void this.send(session, { type: "reconnect_rejected", reason: "Unknown token" });
      session.close("Unknown token");
      return;
    }
    if (!this.disconnectedSeats.has(pid)) {
      void this.send(session, {
        type: "reconnect_rejected",
        reason: "No grace window active for this seat",
      });
      session.close("Not in grace");
      return;
    }

    const grace = this.disconnectedSeats.get(pid)!;
    if (grace.timer !== null) clearTimeout(grace.timer);
    this.disconnectedSeats.delete(pid);
    this.guestSessions.set(pid, session);

    // Wire subsequent messages from this guest.
    session.onMessage((msg) => this.handleGuestMessage(pid as PlayerId, msg));

    // Send fresh state to the reconnecting guest.
    void (async () => {
      let state: GameState;
      let legalResult: LegalActionsResult;
      if (this.nativeBridge) {
        const snapshot = this.nativeBridge.viewerSnapshot(pid as PlayerId);
        state = snapshot.state;
        legalResult = snapshot.legalResult;
      } else {
        const snapshot = await this.wasm.getViewerSnapshot(pid as PlayerId);
        state = snapshot.state;
        legalResult = snapshot;
      }
      await this.send(session, {
        type: "reconnect_ack",
        wireProtocolVersion: WIRE_PROTOCOL_VERSION,
        assignedPlayerId: pid as PlayerId,
        revision: this.authoritativeRevision,
        state,
        playerNames: this.playerNamesForSeats(),
        ...legalActionsToWire(legalResult),
      });
      if (this.deliveredNativeAiDriverFault !== null) {
        await this.send(session, {
          type: "ai_driver_fault",
          ...this.deliveredNativeAiDriverFault,
        });
      }
      if (this.terminalResult !== null) {
        const result = await this.terminalResultForRecipient(pid as PlayerId, state);
        await this.send(session, { type: "terminal_result", result });
      }
    })();

    // Notify other guests.
    for (const [otherPid, otherSession] of this.guestSessions) {
      if (otherPid === pid) continue;
      void this.send(otherSession, { type: "player_reconnected", playerId: pid });
    }
    this.emit({ type: "playerReconnected", playerId: pid });

    // Resume if no other seats are paused.
    if (this.disconnectedSeats.size === 0 && this.gameRunState === "paused-disconnect") {
      this.gameRunState = "running";
      for (const [, s] of this.guestSessions) {
        void this.send(s, { type: "game_resumed" });
      }
      this.emit({ type: "gameResumed" });
    }
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
  ): Promise<void> {
    if (!this.ownsAuthority()) return;
    // Cancel any active grace timer for this seat. `timer` may be null if the
    // host already called `holdForReconnect`.
    const grace = this.disconnectedSeats.get(pid);
    if (grace) {
      if (grace.timer !== null) clearTimeout(grace.timer);
      this.disconnectedSeats.delete(pid);
    }
    // Remove the session for self-concede / grace-expiry paths. (The kick
    // path removes its own session before calling concedePlayer so it can
    // send the `kick` wire message first; double-deletion is a no-op here.)
    const session = this.guestSessions.get(pid);
    if (session) {
      this.guestSessions.delete(pid);
      try { session.close("Player conceded"); } catch { /* best-effort */ }
    }
    this.eliminatedSeats.add(pid);
    this.saveSession();
    try {
      const concedeAction = {
        type: "Concede",
        data: { player_id: pid },
      } as unknown as GameAction;
      // Concede's engine guard requires `actor === player_id`. `pid` is both
      // the seat being conceded and the authenticated identity we're acting
      // on behalf of (e.g. grace-expiry or kick).
      const result = this.nativeBridge
        ? await this.nativeBridge.submitAction(concedeAction, pid)
        : await this.wasm.submitAction(concedeAction, pid);
      await this.broadcastStateUpdate(result.events, result.log_entries, reason);
      await this.runAiLoop();
      this.persistAuthoritativeState();
      if (!this.nativeBridge) {
        this.emit({
          type: "stateChanged",
          snapshot: await this.wasm.getSnapshot(),
          events: result.events,
          logEntries: result.log_entries,
        });
      }
      this.emit(
        origin === "kick"
          ? { type: "playerKicked", playerId: pid, reason }
          : { type: "playerConceded", playerId: pid, reason },
      );
    } catch (err) {
      console.error("[P2PHost] concedePlayer failed:", err);
    }
    // Resume game state if this concede unblocked the pause.
    if (
      this.disconnectedSeats.size === 0 &&
      this.gameRunState === "paused-disconnect"
    ) {
      this.gameRunState = "running";
      for (const [, s] of this.guestSessions) {
        void this.send(s, { type: "game_resumed" });
      }
      this.emit({ type: "gameResumed" });
    }
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
    const token = this.playerTokens.get(pid);
    if (token) this.kickedTokens.add(token);
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
    await this.concedePlayer(pid, reason, "kick");
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
    await this.concedePlayer(pid, reason, "conceded");
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

  /** Manually resume (host UI). Only resumes if no seats are still disconnected. */
  requestResume(): void {
    if (!this.ownsAuthority()) return;
    if (
      this.gameRunState === "paused-manual" &&
      this.disconnectedSeats.size === 0
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
  private nextManaPaymentPreviewRequestId = 1;
  private pendingManaPaymentPreviews = new Map<
    number,
    { resolve: (sourceIds: ObjectId[]) => void; reject: (error: Error) => void }
  >();
  private session: PeerSession | null = null;
  /** The current transport becomes authenticated only after its setup ACK. */
  private authenticatedSession: PeerSession | null = null;
  private playerToken: string | null = null;
  private assignedPlayerId: PlayerId | null = null;
  /** Current host lease accepted from game_setup/reconnect_ack. */
  private authority: P2PAuthorityStamp | null = null;
  readonly supportsMatchConcede: true | undefined;
  private matchConcedeSent = false;
  /** Revision of the cached state frame. A terminal result is bound to this
   * exact final state, not merely to the room code. */
  private cachedRevision: number | null = null;
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
    private readonly hostPeer: Peer,
    private readonly hostPeerId: string,
    private readonly initialConn: DataConnection,
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

  private attachSession(conn: DataConnection): void {
    if (this.terminated) {
      conn.close();
      return;
    }
    traceAdapter("Guest", "attach-session", { connOpen: conn.open });
    const session = createPeerSession(conn, {
      onSessionEnd: () => {
        this.handleHostDisconnect(session);
      },
    });
    this.rejectPendingSubmission(
      new AdapterError("P2P_ERROR", "Host disconnected while submitting an action", true),
    );
    this.rejectPendingManaPaymentPreviews(
      new AdapterError("P2P_ERROR", "Host disconnected during mana-payment preview", true),
    );
    this.session = session;
    this.authenticatedSession = null;
    this.matchConcedeSent = false;
    session.onMessage((msg) => this.handleHostMessage(session, msg));
  }

  async initializeGame(): Promise<SubmitResult> {
    return this.gameSetupPromise;
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
      this.pendingResolve = resolve;
      this.pendingReject = reject;
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
      this.pendingResolve = resolve;
      this.pendingReject = reject;
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
    }
    if (!this.acceptsHostAuthority(msg)) return;
    switch (msg.type) {
      case "game_setup": {
        this.authenticatedSession = session;
        this.assignedPlayerId = msg.assignedPlayerId;
        this.playerToken = msg.playerToken;
        if (msg.authority) {
          this.authority = msg.authority;
          void saveP2PSession(this.sessionKey ?? this.hostPeerId, {
            playerToken: msg.playerToken,
            playerId: msg.assignedPlayerId,
            authority: this.authority,
          });
        }
        this.cachedRevision = msg.revision ?? null;
        this.cacheSnapshot(msg.state, legalActionsFromWire(msg));
        this.emit({ type: "playerIdentity", playerId: msg.assignedPlayerId, playerNames: msg.playerNames });
        this.settleGameSetup({ events: msg.events });
        break;
      }
      case "reconnect_ack": {
        this.authenticatedSession = session;
        this.assignedPlayerId = msg.assignedPlayerId;
        if (this.playerToken && msg.authority) {
          this.authority = msg.authority;
          void saveP2PSession(this.sessionKey ?? this.hostPeerId, {
            playerToken: this.playerToken,
            playerId: msg.assignedPlayerId,
            authority: this.authority,
          });
        }
        this.cachedRevision = msg.revision ?? null;
        const reconnectSnapshot = this.cacheSnapshot(msg.state, legalActionsFromWire(msg));
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
        break;
      }
      case "reconnect_rejected": {
        this.terminate();
        this.rejectGameSetup(msg.reason);
        this.emit({ type: "reconnectFailed", reason: msg.reason });
        this.emit({ type: "gameOver", winner: null, reason: msg.reason });
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
        this.cachedRevision = msg.revision ?? null;
        const updateSnapshot = this.cacheSnapshot(msg.state, legalActionsFromWire(msg));
        if (this.pendingResolve) {
          this.pendingResolve({ events: msg.events, log_entries: msg.logEntries });
          this.pendingResolve = null;
          this.pendingReject = null;
        } else {
          this.emit({
            type: "stateChanged",
            snapshot: updateSnapshot,
            events: msg.events,
            logEntries: msg.logEntries,
          });
        }
        break;
      }
      case "action_rejected": {
        if (this.pendingReject) {
          this.pendingReject(
            actionRejectionError(msg.reason),
          );
          this.pendingResolve = null;
          this.pendingReject = null;
        }
        break;
      }
      case "action_noop": {
        if (this.pendingResolve) {
          this.pendingResolve({ events: [], log_entries: [] });
          this.pendingResolve = null;
          this.pendingReject = null;
        }
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
          pending.reject(actionRejectionError(msg.reason));
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

  private rejectPendingSubmission(error: Error): void {
    this.pendingReject?.(error);
    this.pendingResolve = null;
    this.pendingReject = null;
  }

  private acceptsHostAuthority(msg: P2PMessage): boolean {
    if (!msg.authority) {
      // Old room peers cannot emit a lease stamp. Hosts from this build always
      // do, so their stale incarnations are fenced; accepting this shape keeps
      // the additive wire change from breaking an already-open legacy room.
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
      return msg.authority.sessionKey === this.authority.sessionKey
        && msg.authority.hostIncarnation === this.authority.hostIncarnation;
    }
    return this.authority !== null
      && msg.authority.sessionKey === this.authority.sessionKey
      && msg.authority.hostIncarnation === this.authority.hostIncarnation;
  }

  private send(message: P2PMessage): void {
    if (!this.session) return;
    this.session.send({ ...message, ...(this.authority ? { authority: this.authority } : {}) });
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
    this.authenticatedSession = null;
    this.matchConcedeSent = false;
    this.session = null;
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
      const conn = this.hostPeer.connect(this.hostPeerId);
      await new Promise<void>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("connect timed out")), 10_000);
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
