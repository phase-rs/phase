import type {
  AbilityBlockEntry,
  EngineAdapter,
  EngineSnapshot,
  GameAction,
  GameEvent,
  GameLogEntry,
  GameState,
  LegalActionsResult,
  ManaCost,
  ObjectAction,
  ObjectId,
  PlayerId,
  SubmitResult,
  WithheldFrame,
} from "./types";
import type {
  InteractionPreview,
  InteractionPreviewRequest,
  InteractionSubmission,
} from "./generated/interaction";
import { actionRejectionError, AdapterError, AdapterErrorCode, EMPTY_LEGAL_ACTIONS, isActionRejection, nextSnapshotSeq } from "./types";
import type { BracketDeckRequest, BracketEstimate } from "../types/bracketEstimate";
import {
  HandshakeError,
  openPhaseSocket,
  type PhaseSocket,
  type PhaseSocketTransport,
} from "../services/openPhaseSocket";
import { isValidWebSocketUrl } from "../services/serverDetection";
import { captureSubmissionArtPreferences, submitDeckList } from "../services/deckSubmission";
import type {
  DraftPlayerView,
  StandingEntry,
  TournamentFormat,
  PodPolicy,
  DraftKind,
  SharedStackPileDecision,
} from "./draft-adapter";
import { draftViewPresentation } from "./draft-adapter";
import { SubmissionSlot } from "./submissionSlot";
import type { FullSessionKey, ServerInfo } from "./ws-adapter";
import { getSharedAdapter } from "./wasm-adapter";
import type { StateCodecSession } from "./state-codec-runtime";
import {
  STATE_CODEC_PREPARATION_TIMEOUT_MS,
  StateCodecPreparationAbortError,
  prepareStateCodecSession as prepareBoundedStateCodecSession,
  StateCodecAttachment,
  type StateCodecSessionProvider,
} from "../services/stateCodecSocket";

// ── Types ───────────────────────────────────────────────────────────────

export type DraftPhase =
  | "lobby"
  | "drafting"
  | "review"
  | "deckbuilding"
  | "match"
  | "between_rounds"
  | "complete";

/**
 * Client intent for a server-hosted set draft. This is intentionally not the
 * persisted engine `DraftSource`: a Chaos request names candidate sets only,
 * and the native server privately resolves its per-seat pack assignments.
 */
export type DraftSourceIntent =
  | { type: "Uniform"; data: { set_codes: string[] } }
  | { type: "Chaos"; data: { candidate_codes: string[] } };

/** Settings for creating a new server-hosted draft pod. */
export interface CreateDraftSettings {
  displayName: string;
  /**
   * The set filling each booster, in pack order. One entry per pack the pod
   * opens; the same set may fill several, and a one-element list fills every
   * booster (the server repeats the last entry). Mirrors the wire field
   * Legacy UI input for a Uniform source. New callers may pass `source`
   * directly; the adapter always serializes the tagged source boundary.
   */
  setCodes?: string[];
  /** Canonical server source intent. Never contains Chaos assignments. */
  source?: DraftSourceIntent;
  kind: Exclude<DraftKind, "Quick">;
  public: boolean;
  password?: string;
  timerSeconds?: number;
  tournamentFormat: TournamentFormat;
  podPolicy: PodPolicy;
  podSize: number;
}

function draftSourceIntent(settings: CreateDraftSettings): DraftSourceIntent {
  return settings.source ?? {
    type: "Uniform",
    data: { set_codes: settings.setCodes ?? [] },
  };
}

/** Events emitted by ServerDraftAdapter for UI state updates. */
export type ServerDraftAdapterEvent =
  | { type: "serverHello"; info: ServerInfo; compatible: boolean }
  | { type: "waitingForPlayers" }
  | { type: "draftViewUpdated"; view: DraftPlayerView }
  | { type: "matchStarting"; matchId: string; round: number; opponentName: string; gameCode: string }
  | { type: "timerSync"; remainingMs: number }
  | { type: "draftOver"; standings: StandingEntry[] }
  | { type: "draftActionRejected"; reason: string }
  | { type: "gameStateUpdated"; state: GameState; events: GameEvent[]; legalResult: LegalActionsResult; logEntries?: GameLogEntry[] }
  | { type: "gameOver"; winner: PlayerId | null; reason: string }
  | { type: "opponentDisconnected"; graceSeconds: number }
  | { type: "opponentReconnected" }
  | { type: "actionPendingChanged"; pending: boolean }
  | { type: "disconnected" }
  | { type: "reconnected" }
  | { type: "error"; message: string };

type ServerDraftAdapterEventListener = (event: ServerDraftAdapterEvent) => void;

/** The state-bearing fields shared by the `GameStarted` and `StateUpdate` payloads. */
interface StateBearingPayload {
  state: GameState;
  legal_actions?: GameAction[];
  auto_pass_recommended?: boolean;
  end_continuous_effect_offers?: LegalActionsResult["endContinuousEffectOffers"];
  mana_payment_shortcut_actions?: GameAction[];
  spell_costs?: Record<string, ManaCost>;
  legal_actions_by_object?: Record<string, ObjectAction[]>;
  activation_block_reasons?: Record<string, AbilityBlockEntry[]>;
  viewer_interaction?: LegalActionsResult["viewerInteraction"];
  derived?: GameState["derived"];
}

type DraftInit = {
  resolve: () => void;
  reject: (error: Error) => void;
} & (
  | { kind: "create" }
  | { kind: "reconnect"; draftCode: string; playerToken: string }
);

type AttachmentOwner =
  | { type: "init"; init: DraftInit }
  | {
    type: "join";
    resolve: (view: DraftPlayerView) => void;
    reject: (error: Error) => void;
  };

interface DraftAttachment {
  controller: AbortController;
  owner: AttachmentOwner;
  socket: PhaseSocketTransport | null;
}

interface PendingInteractionPreview {
  resolve: (preview: InteractionPreview) => void;
  reject: (error: Error) => void;
}

function fullSessionKeysEqual(
  left: FullSessionKey | null | undefined,
  right: FullSessionKey | null | undefined,
): boolean {
  return left !== null
    && left !== undefined
    && right !== null
    && right !== undefined
    && left.game_code === right.game_code
    && left.generation === right.generation;
}

export interface ServerDraftAdapterOptions {
  /**
   * Supplies the negotiated StateCodecV1 decoder session for an attachment.
   * `undefined` uses the shared engine only if already ready, otherwise starts
   * with snapshots while it warms in the background. Explicit providers retain
   * bounded preparation and may wait for startup. `null` keeps snapshot-only
   * delivery with no newly introduced await before the handshake.
   */
  stateCodecSessionProvider?: StateCodecSessionProvider | null;
}

// ── ServerDraftAdapter ──────────────────────────────────────────────────

/**
 * WebSocket-backed adapter that handles the full server-hosted draft
 * lifecycle: lobby, picking, deckbuilding, match play (via EngineAdapter),
 * between-rounds, and completion — all over a single WebSocket connection.
 *
 * Follows the WebSocketAdapter pattern (openPhaseSocket handshake, phase-
 * gated handleMessage dispatch, promise-based submitAction). Draft-specific
 * messages (DraftCreated, DraftStateUpdate, DraftMatchStart, etc.) are
 * handled alongside the standard game messages (GameStarted, StateUpdate,
 * GameOver, etc.) based on the current `phase`.
 *
 * Per D-05: single adapter, single socket, full lifecycle.
 * Per T-59-09: does NOT send ReportMatchResult on GameOver — server
 * handles match result reporting automatically.
 *
 * Creation completes on DraftCreated; reconnect completes on DraftStateUpdate
 * after installing the authoritative view/phase, not on match progress frames.
 * A new attachment supersedes pending initialization and cancels an attachment
 * whose setup has not been sent; setup-sent draft actions retain their replies.
 * DraftActionRejected rejects initialization without retiring the transport:
 * the seat may already be reattached and a later state update remains valid.
 * No setup-response deadline is imposed; a silent server can leave it pending.
 */
export class ServerDraftAdapter implements EngineAdapter {
  // ── Draft-phase state ──────────────────────────────────────────────
  private phase: DraftPhase = "lobby";
  private draftCode: string | null = null;
  private draftToken: string | null = null;
  private seatIndex: number | null = null;
  private draftView: DraftPlayerView | null = null;

  // ── Game-phase state ───────────────────────────────────────────────
  /**
   * The single cached engine pair, rebuilt (and re-stamped) once per inbound
   * state-bearing message — same pattern as the P2P guest / ws adapters, so
   * `getState`/`getLegalActions` can never straddle two updates.
   */
  private snapshot: EngineSnapshot | null = null;
  /** The transport whose frame produced {@link snapshot}; null when there is none. */
  private snapshotSource: PhaseSocketTransport | null = null;
  /** Newest applied server state revision; an older Full must not replace it. */
  private cachedStateRevision: number | null = null;
  private _playerId: PlayerId | null = null;
  private activeMatchId: string | null = null;
  private _gameCode: string | null = null;
  /** Full match lifetime announced by DraftMatchStart. */
  private activeFullKey: FullSessionKey | null = null;
  /** Full match lifetime whose matching GameStarted has been accepted. */
  private acceptedFullKey: FullSessionKey | null = null;
  /** Only this socket's accepted GameStarted authorizes new game requests. */
  private acceptedGameSocket: PhaseSocketTransport | null = null;

  // ── Infrastructure ─────────────────────────────────────────────────
  private ws: PhaseSocketTransport | null = null;
  /** Current and retired transports remain owned until their permitted replies settle. */
  private readonly sockets = new Set<PhaseSocketTransport>();
  private readonly submission = new SubmissionSlot();
  private nextManaPaymentPreviewRequestId = 1;
  // Each pending preview records the socket it was sent on, so a retired
  // socket's close settles only its own previews.
  private pendingManaPaymentPreviews = new Map<
    number,
    {
      socket: PhaseSocketTransport;
      resolve: (sourceIds: ObjectId[]) => void;
      reject: (error: Error) => void;
    }
  >();
  private pendingInteractionPreviews = new Map<
    PhaseSocketTransport,
    Map<string, PendingInteractionPreview>
  >();
  private draftResolve: ((view: DraftPlayerView) => void) | null = null;
  private draftReject: ((error: Error) => void) | null = null;
  // Meaningful only while the callback pair owns an action.
  private draftActionStage: "PreparingDeck" | "AwaitingReply" = "AwaitingReply";
  private draftActionSocket: PhaseSocketTransport | null = null;
  private draftPick: {
    draftCode: string;
    playerToken: string | null;
    instanceId: string;
    completeSelection: boolean;
  } | null = null;
  private pendingInit: DraftInit | null = null;
  private unfinishedAttachment: DraftAttachment | null = null;
  private listeners: ServerDraftAdapterEventListener[] = [];
  private pingInterval: ReturnType<typeof setInterval> | null = null;
  private disposed = false;
  private _serverInfo: ServerInfo | null = null;
  /** Undefined uses the best-effort shared default; explicit providers prepare before dialing. */
  private readonly stateCodecProvider: StateCodecSessionProvider | null | undefined;
  /**
   * Application-bootstrap authority and recovery state for the current socket,
   * published when its attachment's socket is adopted. A new draft match resets
   * the bootstrap to pending; only an accepted GameStarted completes it.
   */
  private readonly stateCodec = new StateCodecAttachment();
  /** The socket the in-flight game command was sent on, if any. */
  private submissionSocket: PhaseSocketTransport | null = null;
  /** Full match lifetime owning the command, independent of later match bootstraps. */
  private submissionFullKey: FullSessionKey | null = null;
  /** True once this claim was handed to the transport for the owning match. */
  private submissionSent = false;

  constructor(
    private readonly serverUrl: string,
    options: ServerDraftAdapterOptions = {},
  ) {
    this.stateCodecProvider = options.stateCodecSessionProvider;
  }

  // ── Public accessors ───────────────────────────────────────────────

  get currentPhase(): DraftPhase {
    return this.phase;
  }

  get playerId(): PlayerId | null {
    return this._playerId;
  }

  get gameCode(): string | null {
    return this._gameCode;
  }

  get currentDraftView(): DraftPlayerView | null {
    return this.draftView;
  }

  get currentMatchId(): string | null {
    return this.activeMatchId;
  }

  getServerInfo(): ServerInfo | null {
    return this._serverInfo;
  }

  // ── Event subscription ─────────────────────────────────────────────

  onEvent(listener: ServerDraftAdapterEventListener): () => void {
    this.listeners.push(listener);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener);
    };
  }

  private emit(event: ServerDraftAdapterEvent): void {
    for (const listener of this.listeners) {
      listener(event);
    }
  }

  // ── EngineAdapter interface ────────────────────────────────────────

  async initialize(): Promise<void> {
    // No-op — draft lifecycle is driven by createDraft / joinDraft.
    return Promise.resolve();
  }

  async initializeGame(): Promise<SubmitResult> {
    // Server handles game initialization during DraftMatchStart.
    return { events: [] };
  }

  /**
   * A setup frame must be actually *written* before its candidate displaces
   * anything. Transports with an ordered write receipt (gzip encode+send,
   * native IPC) report the real outcome; a raw transport writes synchronously
   * inside `send` and reports through it.
   */
  private async writeSetupFrame(
    socket: PhaseSocketTransport,
    frame: unknown,
    signal?: AbortSignal,
  ): Promise<boolean> {
    const isAborted = () => signal?.aborted === true;
    if (isAborted()) return false;
    if (socket.sendReceipt === undefined) return this.send(frame, socket);
    if (socket.readyState !== WebSocket.OPEN) return false;
    try {
      await socket.sendReceipt(JSON.stringify(frame), signal);
      // A resolved receipt means the frame was written, even if the attachment
      // was cancelled while waiting; the caller decides what that implies.
      return true;
    } catch {
      return false;
    }
  }

  async submitAction(action: GameAction, _actor: PlayerId): Promise<SubmitResult> {
    this.assertNoPendingSubmission();
    if (this.phase !== "match") {
      throw new AdapterError("PHASE_ERROR", "Not in a match phase", false);
    }
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new AdapterError("WS_ERROR", "WebSocket not connected", false);
    }
    this.assertGameReady(this.ws, this.activeFullKey);

    return this.submitGameFrame(
      { type: "Action", data: { action } },
      new AdapterError("WS_CLOSED", "Failed to send action", true),
    );
  }

  async submitInteraction(
    submission: InteractionSubmission,
    _actor: PlayerId,
  ): Promise<SubmitResult> {
    this.assertNoPendingSubmission();
    if (this.phase !== "match") {
      throw new AdapterError("PHASE_ERROR", "Not in a match phase", false);
    }
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new AdapterError("WS_ERROR", "WebSocket not connected", false);
    }
    this.assertGameReady(this.ws, this.activeFullKey);

    return this.submitGameFrame(
      { type: "Interaction", data: { submission } },
      new AdapterError("WS_CLOSED", "Failed to send interaction", true),
    );
  }

  private assertGameReady(
    socket: PhaseSocketTransport | null,
    ownerKey: FullSessionKey | null,
  ): void {
    if (socket !== null && socket === this.ws && this.acceptedGameSocket === socket
      && fullSessionKeysEqual(ownerKey, this.activeFullKey)
      && fullSessionKeysEqual(this.acceptedFullKey, ownerKey)) return;
    throw new AdapterError(
      AdapterErrorCode.ACTION_NOT_SENT,
      "This connection has not accepted the current draft match; the request was not sent.",
      true,
    );
  }

  private assertNoPendingSubmission(): void {
    this.stateCodec.assertNotRecovering();
    if (this.submission.occupied) {
      throw new AdapterError(
        AdapterErrorCode.ACTION_NOT_SENT,
        "Another game submission is pending; this submission was not sent.",
        true,
      );
    }
  }

  private submitGameFrame(frame: object, sendFailure: AdapterError): Promise<SubmitResult> {
    return new Promise<SubmitResult>((resolve, reject) => {
      // Claim before emitting: event listeners run synchronously and may submit
      // again while handling actionPendingChanged.
      const submission = this.submission.claim(
        resolve,
        reject,
        this._serverInfo?.commandCorrelation === "CommandIdV1",
      );
      // Bind the claimed socket and its owning match together BEFORE any
      // listener can run: a synchronous match change must not bless this
      // command with the replacement match.
      const claimedSocket = this.ws;
      const claimedKey = this.activeFullKey;
      this.submissionSocket = claimedSocket;
      this.submissionFullKey = claimedKey;
      this.submissionSent = false;
      try {
        this.emit({ type: "actionPendingChanged", pending: true });
      } catch (error) {
        if (this.submission.release(submission)) {
          this.submissionSocket = null;
          this.submissionFullKey = null;
          this.retireIdleSockets();
        }
        reject(error instanceof Error ? error : new Error(String(error)));
        return;
      }
      // A synchronous listener may close/dispose the adapter or otherwise
      // settle this slot. Do not send after that owner has been released.
      if (!this.submission.owns(submission)) return;
      try {
        // Pending notifications may synchronously announce another match.
        // Never stamp or send with authority revoked by that reentry.
        this.assertGameReady(claimedSocket, claimedKey);
      } catch (error) {
        // Only an owned slot is ours to release; a listener that already
        // settled it keeps its own outcome and notification.
        if (this.submission.release(submission)) {
          this.submissionSocket = null;
          this.submissionFullKey = null;
          this.submissionSent = false;
          this.retireIdleSockets();
          this.emit({ type: "actionPendingChanged", pending: false });
        }
        reject(error instanceof Error ? error : new Error(String(error)));
        return;
      }
      // From here the frame leaves this adapter, so a later match revocation
      // must settle the caller instead of leaving the slot fenced.
      this.submissionSent = true;
      if (!this.send(this.submission.stamp(frame, submission)) && this.submission.release(submission)) {
        this.submissionSocket = null;
        this.submissionFullKey = null;
        this.submissionSent = false;
        this.retireIdleSockets();
        this.emit({ type: "actionPendingChanged", pending: false });
        reject(sendFailure);
      }
    });
  }

  async previewManaPayment(action: GameAction, _actor: PlayerId): Promise<ObjectId[]> {
    if (this.phase !== "match") {
      throw new AdapterError("PHASE_ERROR", "Not in a match phase", false);
    }
    const socket = this.ws;
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      throw new AdapterError("WS_ERROR", "WebSocket not connected", false);
    }
    this.assertGameReady(socket, this.activeFullKey);

    const requestId = this.nextManaPaymentPreviewRequestId++;
    return new Promise<ObjectId[]>((resolve, reject) => {
      this.pendingManaPaymentPreviews.set(requestId, { socket, resolve, reject });
      if (!this.send({ type: "PreviewManaPayment", data: { request_id: requestId, action } })) {
        this.pendingManaPaymentPreviews.delete(requestId);
        reject(new AdapterError("WS_CLOSED", "Failed to send mana-payment preview", true));
        this.retireIdleSockets();
      }
    });
  }

  async previewInteraction(
    request: InteractionPreviewRequest,
    _actor: PlayerId,
  ): Promise<InteractionPreview> {
    if (this.phase !== "match") {
      throw new AdapterError("PHASE_ERROR", "Not in a match phase", false);
    }
    const socket = this.ws;
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      throw new AdapterError("WS_ERROR", "WebSocket not connected", false);
    }
    this.assertGameReady(socket, this.activeFullKey);

    return new Promise<InteractionPreview>((resolve, reject) => {
      let previews = this.pendingInteractionPreviews.get(socket);
      if (!previews) {
        previews = new Map<string, PendingInteractionPreview>();
        this.pendingInteractionPreviews.set(socket, previews);
      }
      previews.set(request.requestId, { resolve, reject });
      // `request` is forwarded VERBATIM — no field is read, reshaped or rebuilt.
      if (!this.send({ type: "PreviewInteraction", data: { request } })) {
        this.takeInteractionPreview(socket, request.requestId);
        reject(new AdapterError("WS_CLOSED", "Failed to send interaction preview", true));
        this.retireIdleSockets();
      }
    });
  }

  async getState(): Promise<GameState> {
    if (!this.snapshot) {
      throw new AdapterError("WS_ERROR", "No game state available", false);
    }
    return this.snapshot.state;
  }

  async getLegalActions(): Promise<LegalActionsResult> {
    return this.snapshot?.legalResult ?? EMPTY_LEGAL_ACTIONS;
  }

  async getSnapshot(): Promise<EngineSnapshot> {
    if (!this.snapshot) {
      throw new AdapterError("WS_ERROR", "No game state available", false);
    }
    return this.snapshot;
  }

  /** Rebuild the cached pair from an inbound state-bearing message, stamping
   *  it with a fresh globally-monotonic seq at arrival. */
  private cacheSnapshot(
    state: GameState,
    legalResult: LegalActionsResult,
    source: PhaseSocketTransport,
  ): EngineSnapshot {
    this.snapshot = { state, legalResult, seq: nextSnapshotSeq() };
    this.snapshotSource = source;
    return this.snapshot;
  }

  /** Drop the cached pair together with the record of which transport produced it. */
  private clearSnapshot(): void {
    this.snapshot = null;
    this.snapshotSource = null;
    this.cachedStateRevision = null;
  }

  /** Turn a state-bearing payload into the pair {@link cacheSnapshot} stores. */
  private static snapshotPair(
    data: StateBearingPayload,
  ): { state: GameState; legalResult: LegalActionsResult } {
    return {
      state: { ...data.state, derived: data.derived ?? data.state.derived },
      legalResult: {
        actions: data.legal_actions ?? [],
        autoPassRecommended: data.auto_pass_recommended ?? false,
        endContinuousEffectOffers: data.end_continuous_effect_offers ?? [],
        manaPaymentShortcutActions: data.mana_payment_shortcut_actions ?? [],
        spellCosts: data.spell_costs,
        legalActionsByObject: data.legal_actions_by_object,
        activationBlockReasons: data.activation_block_reasons,
        viewerInteraction: data.viewer_interaction,
      },
    };
  }

  restoreState(): void {
    throw new AdapterError("WASM_ERROR", "Undo not supported in server draft", false);
  }

  estimateBracket(_deck: BracketDeckRequest): Promise<BracketEstimate | null> {
    throw new AdapterError(
      AdapterErrorCode.BRACKET_ESTIMATION_UNSUPPORTED,
      "Bracket estimation is a local feature; not available in server draft sessions.",
      false,
    );
  }

  // ── Draft lifecycle methods ────────────────────────────────────────

  async createDraft(settings: CreateDraftSettings): Promise<void> {
    if (!isValidWebSocketUrl(this.serverUrl)) {
      throw new AdapterError("WS_ERROR", "Invalid WebSocket URL", false);
    }
    return new Promise<void>((resolve, reject) => {
      this.beginAttachment({ type: "init", init: { kind: "create", resolve, reject } }, {
        type: "CreateDraftWithSettings",
        data: {
          display_name: settings.displayName,
          source: draftSourceIntent(settings),
          kind: settings.kind,
          public: settings.public,
          password: settings.password ?? null,
          timer_seconds: settings.timerSeconds ?? null,
          tournament_format: settings.tournamentFormat,
          pod_policy: settings.podPolicy,
          pod_size: settings.podSize,
        },
      });
    });
  }

  /**
   * Claim the single `draftResolve`/`draftReject` pair for one action.
   *
   * THE PAIR IS ONE SLOT, AND EVERY ACTION WANTED IT. `joinDraft`, `submitPick`,
   * `submitSharedStackDecision` and `submitDeck` each assigned straight into it.
   * A second action overwrote the first's callbacks, and the next
   * `DraftStateUpdate` resolved only the survivor and nulled both -- so the
   * first caller's promise never settled at all. Not a lost result: a
   * permanently pending `await`, with whatever UI awaited it stuck behind it.
   *
   * Single-flight rather than request correlation, because the wire carries no
   * correlation id: `DraftAction` frames are unlabelled and the server answers
   * with a bare `DraftStateUpdate`. Adding an id is a protocol change; refusing
   * to have two in flight is not, and these actions are user-initiated and
   * mutually exclusive by phase anyway. The refusal is explicit and immediate,
   * which is the part that matters -- the caller learns now instead of awaiting
   * forever.
   *
   * The callback pair is the pending authority; settlement clears it together
   * with its socket owner before retiring an otherwise idle attachment.
   */
  private claimDraftAction(
    action: string,
    resolve: (view: DraftPlayerView) => void,
    reject: (error: Error) => void,
    stage: "PreparingDeck" | "AwaitingReply" = "AwaitingReply",
  ): boolean {
    if (this.draftResolve !== null || this.draftReject !== null) {
      reject(new AdapterError(
        "PHASE_ERROR",
        `Another draft action is still in flight; ${action} was not sent`,
        false,
      ));
      return false;
    }
    this.draftResolve = resolve;
    this.draftReject = reject;
    this.draftActionStage = stage;
    this.draftActionSocket = this.ws;
    return true;
  }

  async joinDraft(
    draftCode: string,
    displayName: string,
    password?: string,
  ): Promise<DraftPlayerView> {
    return new Promise<DraftPlayerView>((resolve, reject) => {
      if (!this.claimDraftAction("JoinDraft", resolve, reject)) return;

      if (!isValidWebSocketUrl(this.serverUrl)) {
        this.settleDraftAction(new AdapterError("WS_ERROR", "Invalid WebSocket URL", false));
        return;
      }

      this.beginAttachment({ type: "join", resolve, reject }, {
        type: "JoinDraftWithPassword",
        data: {
          draft_code: draftCode,
          display_name: displayName,
          password: password ?? null,
        },
      });
    });
  }

  async submitPick(cardInstanceId: string): Promise<DraftPlayerView> {
    if (this.seatIndex === null || this.draftCode === null) {
      throw new AdapterError("PHASE_ERROR", "Not in a draft session", false);
    }
    const view = this.draftView;
    if (view?.pool.some((card) => card.instance_id === cardInstanceId)) {
      // An already-owned instance can never be the outcome of this request,
      // and a later broadcast (any source) would otherwise look like one.
      // Refuse before claiming the slot so nothing is sent.
      throw new AdapterError("PHASE_ERROR", "That card is already in your draft pool", false);
    }
    const draftCode = this.draftCode;
    const playerToken = this.draftToken;
    return new Promise<DraftPlayerView>((resolve, reject) => {
      if (!this.claimDraftAction("Pick", resolve, reject)) return;
      this.draftPick = {
        draftCode,
        playerToken,
        instanceId: cardInstanceId,
        completeSelection: view?.required_pick_count === 1
          && view.current_pack?.some((card) => card.instance_id === cardInstanceId) === true
          && !view.pool.some((card) => card.instance_id === cardInstanceId),
      };
      const sent = this.send({
        type: "DraftAction",
        data: {
          draft_code: this.draftCode,
          action: {
            type: "Pick",
            data: { seat: this.seatIndex, card_instance_ids: [cardInstanceId] },
          },
        },
      });
      if (!sent) {
        this.settleDraftAction(new AdapterError("WS_CLOSED", "Failed to send draft action", true));
      }
    });
  }

  /**
   * One whole shared-stack turn decision on the server-authoritative path.
   *
   * This exists because `CreateDraftSettings.kind` is
   * `Exclude<DraftKind, "Quick">`, which admits `"Winston"` the moment the
   * union widens — a creatable kind with no way to send its only action would
   * be a half-extension. No shipped UI drives this path today (a P2P pod is
   * where Winston is played), but a wire client and any future UI use it.
   *
   * `pile` is the optimistic-concurrency check against the engine's cursor;
   * legality is `shared_stack::refusal_for`'s, server-side.
   */
  async submitSharedStackDecision(
    pile: number,
    decision: SharedStackPileDecision,
  ): Promise<DraftPlayerView> {
    if (this.seatIndex === null || this.draftCode === null) {
      throw new AdapterError("PHASE_ERROR", "Not in a draft session", false);
    }
    return new Promise<DraftPlayerView>((resolve, reject) => {
      if (!this.claimDraftAction("SharedStackDecision", resolve, reject)) return;
      const sent = this.send({
        type: "DraftAction",
        data: {
          draft_code: this.draftCode,
          action: {
            type: "SharedStackDecision",
            data: { seat: this.seatIndex, pile, decision },
          },
        },
      });
      if (!sent) {
        this.settleDraftAction(new AdapterError("WS_CLOSED", "Failed to send draft action", true));
      }
    });
  }

  /**
   * Submit names with optional owner printings selected from one snapshot of
   * the submitter's pins/chain. Only main-deck cards are decorated, including
   * commander-labelled main cards; pool art is not an entry source. Empty or
   * timed-out decoration omits the sidecar, replacing previous submission art.
   * Preparation reserves the action slot and cannot be acknowledged by a
   * broadcast. After sending, acknowledgements remain uncorrelated as before.
   */
  async submitDeck(mainDeck: string[], commanders: string[]): Promise<DraftPlayerView> {
    if (this.seatIndex === null || this.draftCode === null) {
      throw new AdapterError("PHASE_ERROR", "Not in a draft session", false);
    }
    return new Promise<DraftPlayerView>((resolve, reject) => {
      if (!this.claimDraftAction("SubmitDeck", resolve, reject, "PreparingDeck")) return;
      const ws = this.ws;
      const draftCode = this.draftCode;
      const draftToken = this.draftToken;
      const seat = this.seatIndex;
      const prepare = async () => {
        const capturedMain = [...mainDeck];
        const capturedCommanders = [...commanders];
        const snapshot = captureSubmissionArtPreferences();
        const deck = await submitDeckList({ main_deck: capturedMain, sideboard: [] }, snapshot);
        if (this.draftResolve !== resolve || this.draftReject !== reject) return;
        if (this.disposed || this.ws !== ws || !ws || ws.readyState !== WebSocket.OPEN
            || this.draftCode !== draftCode || this.draftToken !== draftToken || this.seatIndex !== seat) {
          throw new AdapterError("WS_CLOSED", "Draft session changed during deck preparation", true);
        }
        this.draftActionStage = "AwaitingReply";
        const sent = this.send({
          type: "DraftAction",
          data: {
            draft_code: draftCode,
            action: {
              type: "SubmitDeck",
              // Key order matches the Rust carrier; absent art keeps old bytes.
              data: {
                seat, main_deck: deck.main_deck, commanders: capturedCommanders,
                ...(deck.printings?.length ? { printings: deck.printings } : {}),
              },
            },
          },
        });
        if (!sent) throw new AdapterError("WS_CLOSED", "Failed to send draft action", true);
      };
      void prepare().catch((error: unknown) => {
        if (this.draftResolve !== resolve || this.draftReject !== reject) return;
        this.settleDraftAction(error instanceof Error ? error : new Error(String(error)));
      });
    });
  }

  // ── Socket management ──────────────────────────────────────────────

  /** Only actual pending work retains a retired transport, never a stale source pointer. */
  private ownsSocketReply(socket: PhaseSocketTransport): boolean {
    if (this.unfinishedAttachment?.socket === socket) return true;
    if (this.submission.occupied && this.submissionSocket === socket) return true;
    if (this.draftReject !== null && this.draftActionStage === "AwaitingReply"
      && this.draftActionSocket === socket) return true;
    for (const pending of this.pendingManaPaymentPreviews.values()) {
      if (pending.socket === socket) return true;
    }
    return this.pendingInteractionPreviews.has(socket);
  }

  /** Remove ownership and callbacks before close: native close may synchronously reenter. */
  private releaseSocket(socket: PhaseSocketTransport, close = true): void {
    if (!this.sockets.delete(socket)) return;
    if (this.acceptedGameSocket === socket) this.acceptedGameSocket = null;
    socket.onmessage = null;
    socket.onerror = null;
    socket.onclose = null;
    if (close) socket.close();
  }

  private retireIdleSockets(): void {
    for (const socket of this.sockets) {
      if (socket !== this.ws && !this.ownsSocketReply(socket)) this.releaseSocket(socket);
    }
  }

  private settleDraftAction(result: DraftPlayerView | Error, onlySocket?: PhaseSocketTransport): boolean {
    if (!this.draftResolve || !this.draftReject
      || (onlySocket && this.draftActionSocket !== onlySocket)) return false;
    const resolve = this.draftResolve;
    const reject = this.draftReject;
    this.draftResolve = null;
    this.draftReject = null;
    this.draftActionSocket = null;
    this.draftPick = null;
    if (result instanceof Error) reject(result);
    else resolve(result);
    this.retireIdleSockets();
    return true;
  }

  /**
   * A pick sent without the engine's complete selection can only be proven by
   * its own sending socket's inventory. Once a different socket is adopted the
   * server's success fan-out follows the registered sender, so the original
   * request may never be answered on its sender. Retire it as unconfirmed
   * (recoverable) instead of leaving the caller awaiting forever.
   */
  private retireUnprovablePick(replacement: PhaseSocketTransport): void {
    if (this.draftReject === null || this.draftActionStage !== "AwaitingReply") return;
    const pick = this.draftPick;
    if (!pick || pick.completeSelection || this.draftActionSocket === replacement) return;
    this.settleDraftAction(new AdapterError(
      "WS_CLOSED",
      "The draft connection changed before this pick was confirmed",
      true,
    ));
  }

  /**
   * Draft broadcasts are not acknowledgements. A pick completes when its
   * inventory proves the exact newly selected instance. A request backed by the
   * complete selection offered in the original view can complete from any
   * source with the captured draft identity; a request without that authority
   * can only be completed by its own sending socket, which is the
   * request-scoped receipt and never a replacement's snapshot.
   * Counter/phase changes and reply ownership alone are not outcome evidence.
   * The wire cannot distinguish a timer selecting that same instance.
   */
  private settleDraftView(
    view: DraftPlayerView,
    sourceSocket: PhaseSocketTransport,
    initOwner: DraftInit | null,
  ): void {
    if (this.draftActionStage !== "AwaitingReply") return;
    const pick = this.draftPick;
    if (pick) {
      if (!view.pool.some((card) => card.instance_id === pick.instanceId)) return;
      const ownsAction = this.draftActionSocket === sourceSocket;
      if (!ownsAction) {
        const sameDraft = initOwner?.kind === "reconnect"
          && initOwner.draftCode === pick.draftCode
          && initOwner.playerToken === pick.playerToken;
        if (!pick.completeSelection || !sameDraft) return;
      }
    }
    // Non-pick actions retain the legacy current-stream completion semantics;
    // retired frames have already been restricted to their own pending reply.
    this.settleDraftAction(view);
  }

  private takeInteractionPreview(socket: PhaseSocketTransport, requestId: string) {
    const previews = this.pendingInteractionPreviews.get(socket);
    if (!previews) return null;
    const pending = previews.get(requestId);
    if (pending) {
      previews.delete(requestId);
      if (previews.size === 0) this.pendingInteractionPreviews.delete(socket);
    }
    return pending ?? null;
  }

  /** Legacy current-stream replies still belong to the command's match. */
  private matchesSubmissionMatch(source: PhaseSocketTransport): boolean {
    return source !== this.ws || !this.submission.occupied
      || fullSessionKeysEqual(this.submissionFullKey, this.activeFullKey);
  }

  /** Preserve legacy current-stream acknowledgement while fencing stamped and retired sources. */
  private takeSubmission(source: PhaseSocketTransport, commandId: unknown) {
    if (!this.matchesSubmissionMatch(source)) return null;
    if (this.submissionSocket !== source && (this.submission.stamped || source !== this.ws)) return null;
    const pending = this.submission.take(commandId);
    if (pending) {
      this.submissionSocket = null;
      this.submissionFullKey = null;
      this.submissionSent = false;
    }
    return pending;
  }

  private settleInit(expected: DraftInit | null, error?: Error): boolean {
    if (expected === null || this.pendingInit !== expected) return false;
    this.pendingInit = null;
    if (error) expected.reject(error);
    else expected.resolve();
    return true;
  }

  private rejectAttachment(owner: AttachmentOwner, error: Error): void {
    switch (owner.type) {
      case "init":
        this.settleInit(owner.init, error);
        break;
      case "join":
        if (this.draftResolve === owner.resolve && this.draftReject === owner.reject) {
          this.settleDraftAction(error);
        }
        break;
    }
  }

  private cancelAttachment(error: Error): void {
    const attachment = this.unfinishedAttachment;
    if (!attachment) return;
    this.unfinishedAttachment = null;
    this.rejectAttachment(attachment.owner, error);
    attachment.controller.abort(error);
  }

  private beginAttachment(owner: AttachmentOwner, setupFrame: unknown): void {
    const superseded = new AdapterError("WS_CLOSED", "Draft attachment superseded by a new operation", true);
    this.settleInit(this.pendingInit, superseded);
    this.cancelAttachment(superseded);
    if (owner.type === "init") this.pendingInit = owner.init;
    const attachment: DraftAttachment = { owner, controller: new AbortController(), socket: null };
    this.unfinishedAttachment = attachment;
    // attachSocket settles the owner for transport/listener failures; this
    // final catch only keeps an exception thrown after settlement (e.g. by a
    // protocol-mismatch diagnostic listener) from becoming an unhandled
    // rejection.
    void this.attachSocket(attachment, setupFrame).catch((error: unknown) => {
      this.rejectAttachment(owner, error instanceof Error ? error : new Error(String(error)));
    });
  }

  private ownsAttachment(attachment: DraftAttachment): boolean {
    return this.unfinishedAttachment === attachment
      && !attachment.controller.signal.aborted
      // Draft replies are uncorrelated: another socket can release a join's
      // pair before its handshake resumes. It still must send the claimed join.
      && (attachment.owner.type === "join" || this.pendingInit === attachment.owner.init);
  }

  /**
   * Drops the pending command slot for a codec recovery of the current socket
   * and rejects it with `error`. A stamped command that was claimed on a
   * still-open retired socket is not this recovery's to drop: that socket's
   * completion or close still settles it (see {@link settleRetiredCompletion}
   * and {@link settleSocketClose}). An unstamped (legacy) slot still accepts
   * the current stream's first completion, and a slot with no recorded socket
   * has no retired owner, so both are dropped as before.
   */
  private interruptPendingSubmission(error: AdapterError): void {
    const claimedOnRetiredSocket = this.submission.stamped
      && this.submissionSocket !== null
      && this.submissionSocket !== this.ws;
    if (claimedOnRetiredSocket) return;
    const dropped = this.submission.drop();
    this.submissionSocket = null;
    this.submissionFullKey = null;
    this.submissionSent = false;
    try {
      if (dropped) {
        dropped.reject(error);
        this.emit({ type: "actionPendingChanged", pending: false });
      }
    } finally {
      this.retireIdleSockets();
    }
  }

  /** A close settles only the command, previews and draft action sent on that source. */
  private settleSocketClose(source: PhaseSocketTransport): void {
    // Previews sent on the retired socket can no longer be answered; previews
    // sent on the current socket are not its to reject.
    this.rejectPendingManaPaymentPreviews(
      new AdapterError("WS_CLOSED", "Connection closed during mana-payment preview", true),
      source,
    );
    this.rejectPendingInteractionPreviews(
      new AdapterError("WS_CLOSED", "Connection closed during interaction preview", true),
      source,
    );
    this.settleDraftAction(
      new AdapterError("WS_CLOSED", "Connection closed during draft operation", true),
      source,
    );
    if (this.submissionSocket !== source) return;
    const dropped = this.submission.drop();
    this.submissionSocket = null;
    this.submissionFullKey = null;
    this.submissionSent = false;
    if (!dropped) return;
    dropped.reject(new AdapterError("WS_CLOSED", "Connection closed during action", true));
    this.emit({ type: "actionPendingChanged", pending: false });
  }

  /**
   * A retired socket's StateUpdate is withheld from game delivery, but it still
   * answers the command that was claimed on it, exactly as it did before the
   * replacement. Callers read the snapshot as soon as the command resolves, so
   * the reply's own state is cached first, but only for the same active match
   * while the replacement has delivered no game state. A later match must not
   * borrow this reply as its empty snapshot. The reply never emits a game event
   * and never touches the accepted Full-session key.
   */
  private settleRetiredCompletion(
    retired: PhaseSocketTransport,
    msg: { data?: unknown; command_id?: unknown },
  ): void {
    if (this.submissionSocket !== retired) return;
    const data = (msg.data ?? {}) as Partial<StateBearingPayload> & {
      events?: GameEvent[];
      log_entries?: GameLogEntry[];
      full_key?: FullSessionKey;
    };
    // Completion belongs to the command's match, not the replacement's current
    // bootstrap (which may be empty or already accepting another Full key).
    if (!fullSessionKeysEqual(data.full_key, this.submissionFullKey)) return;
    const pending = this.takeSubmission(retired, msg.command_id);
    if (!pending) return;
    if (data.state && fullSessionKeysEqual(data.full_key, this.activeFullKey)
      && this.snapshotSource !== this.ws) {
      const { state, legalResult } = ServerDraftAdapter.snapshotPair(
        data as StateBearingPayload,
      );
      this.cacheSnapshot(state, legalResult, retired);
    }
    pending.resolve({ events: data.events ?? [], log_entries: data.log_entries });
    this.emit({ type: "actionPendingChanged", pending: false });
  }

  /**
   * The server replaces a seat's sender on `ReconnectDraft` and never closes
   * the predecessor, so after that frame is written the incumbent no longer
   * receives the seat's broadcasts. A candidate cancelled or closed past that
   * point must not leave the incumbent as the current socket: it stops being
   * current and is released unless it still owes a reply (the same contract a
   * replaced socket has after a successful adoption). Create and Join attach a
   * different seat and leave the incumbent's seat untouched.
   */
  private retireSwappedIncumbent(candidate: PhaseSocketTransport, owner: AttachmentOwner): void {
    if (owner.type !== "init" || owner.init.kind !== "reconnect") return;
    const incumbent = this.ws;
    if (incumbent === null || incumbent === candidate) return;
    this.ws = null;
    if (this.pingInterval) {
      clearInterval(this.pingInterval);
      this.pingInterval = null;
    }
    this.stateCodec.clearRecovery();
    this.retireIdleSockets();
  }

  private abandonSocket(socket: PhaseSocket): void {
    // Invalidate the candidate before notifications can reenter on a successor.
    if (this.ws === socket.ws) {
      this.ws = null;
      if (this.pingInterval) {
        clearInterval(this.pingInterval);
        this.pingInterval = null;
      }
    }
    try {
      this.settleSocketClose(socket.ws);
    } finally {
      // Source-scoped settlement must not strand the lease if a listener throws.
      this.releaseSocket(socket.ws);
    }
  }

  /**
   * Ownership spans codec preparation, handshake completion, setup send and
   * synchronous hello listeners; replaced sockets retain only permitted pending replies.
   */
  private async attachSocket(attachment: DraftAttachment, setupFrame: unknown): Promise<void> {
    let socket: PhaseSocket | null = null;
    let prepared: StateCodecSession | null = null;
    const initOwner = attachment.owner.type === "init" ? attachment.owner.init : null;
    const bootstrap = this.stateCodec.begin();
    // Published as the adapter's binding only when this attachment's socket is
    // adopted below: until then a still-current older socket keeps its own
    // binding and recovery gate, and `ownsAttachment` tracks supersession.
    try {
      // Only explicit providers prepare before dialing. The shared default
      // races capability discovery without delaying socket creation.
      const provider = this.stateCodec.selectProvider(this.stateCodecProvider);
      if (provider) {
        try {
          prepared = await prepareBoundedStateCodecSession(
            provider,
            attachment.controller.signal,
            STATE_CODEC_PREPARATION_TIMEOUT_MS,
          );
        } catch (error) {
          if (error instanceof StateCodecPreparationAbortError) throw error;
          prepared = null;
        }
      }
      if (!this.ownsAttachment(attachment)) {
        prepared?.close();
        prepared = null;
        return;
      }
      // Ownership of the lease moves into the handshake helper, which releases
      // it on every failure path and on non-negotiation. A lease that arrives
      // after this adapter degraded is closed here and not offered.
      const handed = provider === undefined
        ? getSharedAdapter().openStateCodecSession("ifReady")
          .then((session) => this.stateCodec.selectLease(session))
        : this.stateCodec.selectLease(prepared);
      prepared = null;
      socket = await openPhaseSocket(this.serverUrl, {
        signal: attachment.controller.signal,
        stateCodecSession: handed,
        stateCodecBootstrap: () => this.stateCodec.status(bootstrap, this.ws, this.disposed),
        onStateCodecRecovery: (state) => this.stateCodec.handleRecovery(
          bootstrap, state, this.ws, this.disposed,
          (error) => this.interruptPendingSubmission(error),
        ),
      });
      attachment.socket = socket.ws;
      this.sockets.add(socket.ws);
      if (!this.ownsAttachment(attachment)) {
        this.abandonSocket(socket);
        return;
      }
      const sourceSocket = socket.ws;
      // Do not publish a request-capable candidate until its setup is sent.
      // Raw transports can synchronously reenter during send; retain those
      // messages until both setup ordering and attachment ownership are safe.
      const setupMessages: Parameters<NonNullable<PhaseSocketTransport["onmessage"]>>[0][] = [];
      sourceSocket.onmessage = (event) => { setupMessages.push(event); };
      const handleMessage: NonNullable<PhaseSocketTransport["onmessage"]> = (event) => {
        if (!this.sockets.has(sourceSocket)) return;
        try {
          const message: { type: string; data?: unknown; command_id?: unknown } = JSON.parse(event.data);
          if (this.ws !== sourceSocket) {
            switch (message.type) {
              case "StateUpdate":
                this.settleRetiredCompletion(sourceSocket, message);
                return;
              case "ActionNoOp":
              case "ActionRejected":
              case "ActionFailed":
              case "StateWithheld":
                if (!this.submission.occupied || this.submissionSocket !== sourceSocket) return;
                break;
              case "DraftJoined":
              case "DraftStateUpdate":
              case "DraftActionRejected":
                if (!this.draftReject || this.draftActionStage !== "AwaitingReply"
                  || this.draftActionSocket !== sourceSocket) return;
                break;
              case "ManaPaymentPreview":
              case "ManaPaymentPreviewRejected":
              case "ManaPaymentPreviewFailed":
              case "InteractionPreview":
              case "InteractionPreviewFailed":
                break;
              default:
                return;
            }
          }
          return this.handleMessage(message, initOwner, sourceSocket);
        } finally {
          this.retireIdleSockets();
        }
      };
      sourceSocket.onerror = () => {
        if (!this.sockets.has(sourceSocket)) return;
        try {
          const err = new AdapterError("WS_ERROR", "WebSocket connection failed", true);
          if (this.ws === sourceSocket || this.unfinishedAttachment === attachment) this.settleInit(initOwner, err);
          this.settleDraftAction(err, sourceSocket);
        } finally {
          this.retireIdleSockets();
        }
      };
      sourceSocket.onclose = () => {
        if (!this.sockets.has(sourceSocket)) return;
        const current = this.ws === sourceSocket;
        // The transport already observed raw death; do not close the raw socket again.
        this.releaseSocket(sourceSocket, false);
        if (current && this.pingInterval) {
          clearInterval(this.pingInterval);
          this.pingInterval = null;
        }
        if (current) this.stateCodec.clearRecovery();
        // Settle initialization before synchronous notifications can throw.
        const settledOwn = (current || this.unfinishedAttachment === attachment)
          && this.settleInit(initOwner, new AdapterError("WS_CLOSED", "Connection closed before draft started", true));
        try {
          this.settleSocketClose(sourceSocket);
          if (current && !settledOwn && this.pendingInit === null && this.draftToken && !this.disposed) {
            this.emit({ type: "disconnected" });
          }
        } finally {
          this.retireIdleSockets();
        }
      };

      if (attachment.owner.type === "join"
        && this.draftResolve === attachment.owner.resolve && this.draftReject === attachment.owner.reject) {
        this.draftActionSocket = sourceSocket;
      }
      if (!this.ownsAttachment(attachment) || !this.sockets.has(sourceSocket)) {
        // A terminal codec failure may have arrived during ClientHello. Consume
        // that binding's sticky delivery policy without displacing an incumbent
        // peer: this candidate never owned a setup-capable attachment.
        if (!this.disposed && bootstrap.failure !== null) {
          this.stateCodec.consumeFailure(bootstrap);
        }
        this.abandonSocket(socket);
        return;
      }
      // Same policy consumption for a live candidate; the incumbent socket and
      // its credentials stay current until this candidate's setup is on the wire.
      if (bootstrap.failure !== null) {
        this.stateCodec.consumeFailure(bootstrap);
      }
      // Address the local candidate, not the still-current previous socket.
      if (!(await this.writeSetupFrame(sourceSocket, setupFrame, attachment.controller.signal))) {
        throw new AdapterError("WS_CLOSED", "Failed to send setup frame", true);
      }
      if (!this.ownsAttachment(attachment) || !this.sockets.has(sourceSocket)) {
        // The frame is on the wire: a ReconnectDraft has already moved the
        // seat's sender to this candidate, so the incumbent is deaf.
        this.retireSwappedIncumbent(sourceSocket, attachment.owner);
        this.abandonSocket(socket);
        return;
      }
      // Adoption retires the previous binding only after setup is on the wire.
      this.stateCodec.adopt(bootstrap, sourceSocket);
      this.ws = sourceSocket;
      this.acceptedGameSocket = null;
      // A pick awaiting proof on the replaced socket can no longer be
      // confirmed here: retire it before any listener observes the successor.
      this.retireUnprovablePick(sourceSocket);
      if (initOwner?.kind !== "reconnect") {
        // Create/Join attach a new draft seat, not the previous draft's Full
        // game. Only replies on this attachment may establish its authority.
        // Pending A work keeps its captured source/key until its own reply.
        this.phase = "lobby";
        this.draftCode = null;
        this.draftToken = null;
        this.seatIndex = null;
        this.draftView = null;
        this.clearSnapshot();
        this._playerId = null;
        this.activeMatchId = null;
        this._gameCode = null;
        this.activeFullKey = null;
        this.acceptedFullKey = null;
      }
      this._serverInfo = socket.serverInfo;
      this.retireIdleSockets();
      this.emit({ type: "serverHello", info: socket.serverInfo, compatible: true });
      // A setup reply may already have settled initialization. Attachment
      // identity, not an unsettled initializer, owns the remaining handoff.
      if (this.unfinishedAttachment !== attachment || attachment.controller.signal.aborted
        || this.ws !== sourceSocket) {
        this.abandonSocket(socket);
        return;
      }
      this.startPing();
      // The buffer stays installed through hello callbacks and this whole
      // drain: their synchronous replies must follow the frames admitted before
      // the candidate was published. Index, not an iterator, so appended
      // replies are drained in wire order.
      for (let index = 0; index < setupMessages.length; index++) {
        if (!this.sockets.has(sourceSocket)) return;
        handleMessage(setupMessages[index]!);
      }
      if (!this.sockets.has(sourceSocket)) return;
      sourceSocket.onmessage = handleMessage;
    } catch (error) {
      const failure = error instanceof HandshakeError
        ? new AdapterError("WS_ERROR", error.message, error.kind !== "protocol_mismatch" && error.kind !== "invalid_url")
        // Before a socket exists the failure is an acquisition failure, and the
        // caller gets the normalized WS_ERROR the base adapter produced.
        // After acquisition it is a listener/setup failure and keeps its
        // identity (the setup-send path already throws an AdapterError).
        : socket === null
          ? new AdapterError("WS_ERROR", String(error), true)
          : error instanceof Error ? error : new Error(String(error));
      this.rejectAttachment(attachment.owner, failure);
      if (socket) {
        if (!this.disposed && bootstrap.failure !== null) {
          this.stateCodec.consumeFailure(bootstrap);
        }
        this.abandonSocket(socket);
      }
      if (error instanceof HandshakeError
        && error.kind === "protocol_mismatch" && error.serverInfo
        && this.unfinishedAttachment === attachment && !attachment.controller.signal.aborted) {
        this._serverInfo = error.serverInfo;
        this.emit({ type: "serverHello", info: error.serverInfo, compatible: false });
      }
    } finally {
      if (this.unfinishedAttachment === attachment) this.unfinishedAttachment = null;
    }
  }

  // ── Message handling ───────────────────────────────────────────────

  private handleMessage(
    msg: { type: string; data?: unknown; command_id?: unknown },
    initOwner: DraftInit | null,
    sourceSocket: PhaseSocketTransport,
  ): true | void {
    switch (msg.type) {
      // ── Draft-phase messages ──────────────────────────────────────
      case "DraftCreated": {
        const data = msg.data as {
          draft_code: string;
          player_token: string;
          seat_index: number;
        };
        this.draftCode = data.draft_code;
        this.draftToken = data.player_token;
        this.seatIndex = data.seat_index;
        if (initOwner?.kind === "create" && this.ws === sourceSocket) this.settleInit(initOwner);
        this.emit({ type: "waitingForPlayers" });
        break;
      }

      case "DraftJoined": {
        const data = msg.data as {
          draft_code: string;
          player_token: string;
          seat_index: number;
          view: DraftPlayerView;
        };
        if (this.ws === sourceSocket) {
          this.draftCode = data.draft_code;
          this.draftToken = data.player_token;
          this.seatIndex = data.seat_index;
          this.draftView = data.view;
          this.updatePhaseFromView(data.view);
        }
        this.settleDraftView(data.view, sourceSocket, initOwner);
        break;
      }

      case "DraftStateUpdate": {
        const data = msg.data as { view: DraftPlayerView };
        const current = this.ws === sourceSocket;
        if (current) {
          this.draftView = data.view;
          this.updatePhaseFromView(data.view);
          if (initOwner?.kind === "reconnect") this.settleInit(initOwner);
        }
        // Reconnect captures B's bootstrap after installing its sender. An A
        // selection accepted before that cutover is already in B's pool; a
        // later A selection is rejected on A. Either stream must prove the
        // captured selection, and A can settle only its reply, never B's view.
        this.settleDraftView(data.view, sourceSocket, initOwner);
        if (current) this.emit({ type: "draftViewUpdated", view: data.view });
        break;
      }

      case "DraftMatchStart": {
        const data = msg.data as {
          match_id: string;
          round: number;
          game_code: string;
          full_key?: FullSessionKey;
          player_token: string;
          your_player: PlayerId;
          opponent_name: string;
        };
        if (
          !data.full_key
          || data.full_key.game_code !== data.game_code
          || data.full_key.generation < 1
        ) {
          this.emit({
            type: "error",
            message: "Server omitted a valid Full session identity for the draft match.",
          });
          break;
        }
        const sameMatch =
          this.activeMatchId === data.match_id
          && this._gameCode === data.game_code
          && this._playerId === data.your_player
          && fullSessionKeysEqual(this.activeFullKey, data.full_key);
        if (!sameMatch) {
          this.clearSnapshot();
          this.acceptedFullKey = null;
          this.acceptedGameSocket = null;
          // A new match needs its own GameStarted: reset before any listener
          // can observe the transition.
          this.stateCodec.reset(sourceSocket);
        }
        // Publish the new match authority BEFORE any listener can observe the
        // transition. A synchronous retry from the pending callback must not be
        // stamped for the outgoing match, whose addressed reply is fenced by
        // the new key published here.
        this.phase = "match";
        this.activeMatchId = data.match_id;
        this._playerId = data.your_player;
        this._gameCode = data.game_code;
        this.draftToken = data.player_token;
        this.activeFullKey = data.full_key;
        let releasedPending = false;
        if (!sameMatch) {
          // A command already SENT for the outgoing match on this same socket
          // can never complete: its addressed reply is fenced by the new match
          // key above. Release the slot before listeners can observe it; a
          // command still being claimed is released unsent by its own recheck.
          // The notification itself is deferred until this transition's own
          // setup frame is on the wire, so a throwing subscriber cannot skip it.
          if (this.submission.occupied && this.submissionSocket === sourceSocket
            && this.submissionSent) {
            const revoked = this.submission.drop();
            this.submissionSocket = null;
            this.submissionFullKey = null;
            this.submissionSent = false;
            if (revoked) {
              revoked.reject(new AdapterError(
                "REQUEST_FAILED",
                "The draft match changed before this command completed",
                true,
              ));
              releasedPending = true;
            }
          }
        }
        if (!sameMatch) {
          try {
            if (this.draftCode && this.draftToken) {
              this.send({
                type: "ReconnectDraft",
                data: {
                  draft_code: this.draftCode,
                  player_token: this.draftToken,
                },
              });
            } else {
              this.emit({
                type: "error",
                message: "Cannot attach draft match without draft credentials.",
              });
            }
            this.emit({
              type: "matchStarting",
              matchId: data.match_id,
              round: data.round,
              opponentName: data.opponent_name,
              gameCode: data.game_code,
            });
          } finally {
            // The pending clear is the transition's guaranteed completion: a
            // throwing matchStarting subscriber must not suppress it, and a
            // repeated announcement cannot replay it later.
            if (releasedPending) {
              this.emit({ type: "actionPendingChanged", pending: false });
            }
          }
        }
        break;
      }

      case "DraftTimerSync": {
        const data = msg.data as { remaining_ms: number };
        this.emit({ type: "timerSync", remainingMs: data.remaining_ms });
        break;
      }

      case "DraftActionRejected": {
        const data = msg.data as { reason: string };
        if (this.ws === sourceSocket) {
          this.settleInit(initOwner, new AdapterError("ACTION_REJECTED", data.reason, true));
        }
        // The wire carries no correlation id, and preparation has not sent a
        // frame yet: while SubmitDeck is still `PreparingDeck`, a rejection can
        // only answer an EARLIER action, so it must not abort this submission
        // with another action's reason.
        if (this.draftActionStage === "AwaitingReply") {
          this.settleDraftAction(new AdapterError("ACTION_REJECTED", data.reason, true), sourceSocket);
        }
        this.emit({ type: "draftActionRejected", reason: data.reason });
        break;
      }

      case "DraftOver": {
        this.phase = "complete";
        const data = msg.data as { standings: StandingEntry[] };
        this.emit({ type: "draftOver", standings: data.standings });
        break;
      }

      // ── Game-phase messages (mirrors WebSocketAdapter) ─────────────
      case "GameStarted": {
        const data = msg.data as {
          state: GameState;
          your_player: PlayerId;
          state_revision?: number;
          legal_actions?: GameAction[];
          auto_pass_recommended?: boolean;
          end_continuous_effect_offers?: LegalActionsResult["endContinuousEffectOffers"];
          mana_payment_shortcut_actions?: GameAction[];
          spell_costs?: Record<string, ManaCost>;
          legal_actions_by_object?: Record<string, ObjectAction[]>;
          activation_block_reasons?: Record<string, AbilityBlockEntry[]>;
          viewer_interaction?: LegalActionsResult["viewerInteraction"];
          derived?: GameState["derived"];
          full_key?: FullSessionKey;
        };
        if (
          this.activeMatchId === null
          || this._gameCode === null
          || this._playerId !== data.your_player
          || !fullSessionKeysEqual(data.full_key, this.activeFullKey)
        ) {
          break;
        }
        const revision = typeof data.state_revision === "number" ? data.state_revision : null;
        if (this.cachedStateRevision !== null && revision !== null
          && revision < this.cachedStateRevision) {
          // A same-key bootstrap that regresses the applied revision is not
          // accepted evidence: it must not replace the snapshot, release the
          // codec's recovery gate, or authorize commands.
          break;
        }
        this.acceptedFullKey = data.full_key ?? null;
        const started = ServerDraftAdapter.snapshotPair(data);
        const startedSnapshot = this.cacheSnapshot(started.state, started.legalResult, sourceSocket);
        this.cachedStateRevision = revision ?? this.cachedStateRevision;
        this._playerId = data.your_player;
        this.acceptedGameSocket = sourceSocket;
        // Accepted after the match/player/full-key guards above and after the
        // snapshot is cached, but before the listener emit.
        this.stateCodec.accept(sourceSocket);
        this.emit({
          type: "gameStateUpdated",
          state: startedSnapshot.state,
          events: [],
          legalResult: startedSnapshot.legalResult,
        });
        return true;
      }

      case "StateUpdate": {
        const data = msg.data as {
          state: GameState;
          events: GameEvent[];
          state_revision?: number;
          legal_actions?: GameAction[];
          auto_pass_recommended?: boolean;
          end_continuous_effect_offers?: LegalActionsResult["endContinuousEffectOffers"];
          mana_payment_shortcut_actions?: GameAction[];
          spell_costs?: Record<string, ManaCost>;
          legal_actions_by_object?: Record<string, ObjectAction[]>;
          activation_block_reasons?: Record<string, AbilityBlockEntry[]>;
          viewer_interaction?: LegalActionsResult["viewerInteraction"];
          log_entries?: GameLogEntry[];
          derived?: GameState["derived"];
          full_key?: FullSessionKey;
        };
        if (!fullSessionKeysEqual(data.full_key, this.acceptedFullKey)) {
          break;
        }
        if (this.cachedStateRevision !== null && typeof data.state_revision === "number"
          && data.state_revision < this.cachedStateRevision) {
          // A decoded recovery Full older than the applied revision must not
          // replace it, and it is not an accepted bootstrap: the codec recovery
          // gate stays closed until a current revision arrives. It may still
          // answer this socket's own correlated command.
          const stale = this.takeSubmission(sourceSocket, msg.command_id);
          if (stale) {
            stale.resolve({ events: data.events, log_entries: data.log_entries });
            this.emit({ type: "actionPendingChanged", pending: false });
          }
          break;
        }
        const updated = ServerDraftAdapter.snapshotPair(data);
        const updateSnapshot = this.cacheSnapshot(updated.state, updated.legalResult, sourceSocket);
        this.cachedStateRevision = data.state_revision ?? this.cachedStateRevision;
        // A peer's StateUpdate is not an acknowledgement of our command.
        const pending = this.takeSubmission(sourceSocket, msg.command_id);
        if (pending) {
          pending.resolve({ events: data.events, log_entries: data.log_entries });
          this.emit({ type: "actionPendingChanged", pending: false });
        } else {
          this.emit({
            type: "gameStateUpdated",
            state: updateSnapshot.state,
            events: data.events,
            legalResult: updateSnapshot.legalResult,
            logEntries: data.log_entries,
          });
        }
        return true;
      }

      case "ActionRejected": {
        if (!this.matchesSubmissionMatch(sourceSocket)) break;
        const data = msg.data as { rejection?: unknown };
        const pending = this.takeSubmission(sourceSocket, msg.command_id);
        if (pending) {
          // Game-phase action rejection. `ServerDraftAdapter` is a full
          // `EngineAdapter` once the pod's game starts, so it must classify the
          // engine's stale verdicts exactly as the WebSocket and P2P transports
          // do — otherwise a stale `ReorderHand` in a server-hosted draft game
          // still surfaces as the red recoverable error this PR removes
          // everywhere else.
          //
          // The mana-payment preview handler below routes through the same
          // classifier. Deliberately NOT applied to `DraftActionRejected`: that
          // carries a pick/pass rejection, which is not a `GameAction` at all,
          // so no stale-action verdict is possible — it is a separate draft
          // protocol concern and stays a plain recoverable rejection.
          pending.reject(
            isActionRejection(data.rejection)
              ? actionRejectionError(data.rejection)
              : new AdapterError(AdapterErrorCode.WASM_ERROR, "Server sent an invalid action rejection.", false),
          );
          this.emit({ type: "actionPendingChanged", pending: false });
        } else if (this.submission.unmatched(msg.command_id).releasesIndicator) {
          // Legacy servers always released the pending indicator here.
          this.emit({ type: "actionPendingChanged", pending: false });
        }
        break;
      }

      case "ActionFailed": {
        if (!this.matchesSubmissionMatch(sourceSocket)) break;
        const data = msg.data as { message: string };
        const pending = this.takeSubmission(sourceSocket, msg.command_id);
        if (pending) {
          pending.reject(new AdapterError(AdapterErrorCode.REQUEST_FAILED, data.message, true));
          this.emit({ type: "actionPendingChanged", pending: false });
        } else if (this.submission.unmatched(msg.command_id).surfaces) {
          this.emit({ type: "error", message: data.message });
        }
        break;
      }

      case "StateWithheld": {
        if (!this.matchesSubmissionMatch(sourceSocket)) break;
        const data = msg.data as {
          full_key?: FullSessionKey;
          withheld: WithheldFrame;
          reason: string;
        };
        const completionKey = sourceSocket === this.ws ? this.acceptedFullKey : this.submissionFullKey;
        if (data.withheld.type === "StateUpdate" && fullSessionKeysEqual(data.full_key, completionKey)) {
          const pending = this.takeSubmission(sourceSocket, msg.command_id);
          if (pending) {
            pending.resolve({ events: [], log_entries: [] });
            this.emit({ type: "actionPendingChanged", pending: false });
          } else if (this.submission.unmatched(msg.command_id).releasesIndicator) {
            this.emit({ type: "actionPendingChanged", pending: false });
          }
        }
        this.emit({ type: "error", message: data.reason });
        break;
      }

      case "ActionNoOp": {
        if (!this.matchesSubmissionMatch(sourceSocket)) break;
        const pending = this.takeSubmission(sourceSocket, msg.command_id);
        if (pending) {
          pending.resolve({ events: [], log_entries: [] });
          this.emit({ type: "actionPendingChanged", pending: false });
        } else if (this.submission.unmatched(msg.command_id).releasesIndicator) {
          // Legacy servers always released the pending indicator here.
          this.emit({ type: "actionPendingChanged", pending: false });
        }
        break;
      }

      case "ManaPaymentPreview": {
        const data = msg.data as { request_id: number; source_ids: ObjectId[] };
        const pending = this.pendingManaPaymentPreviews.get(data.request_id);
        if (pending?.socket === sourceSocket) {
          this.pendingManaPaymentPreviews.delete(data.request_id);
          pending.resolve(data.source_ids);
        }
        break;
      }

      case "ManaPaymentPreviewRejected": {
        const data = msg.data as { request_id: number; rejection?: unknown };
        const pending = this.pendingManaPaymentPreviews.get(data.request_id);
        if (pending?.socket === sourceSocket) {
          this.pendingManaPaymentPreviews.delete(data.request_id);
          // Same shared classifier as the action path above. A preview is
          // answered against the same engine state an action would be, so it
          // can carry the same stale verdict when the state moves underneath
          // the request — and a stale preview is likewise void rather than
          // retryable. Non-stale reasons still classify as recoverable
          // ACTION_REJECTED, so existing surface/retry behavior is unchanged.
          pending.reject(
            isActionRejection(data.rejection)
              ? actionRejectionError(data.rejection)
              : new AdapterError(AdapterErrorCode.WASM_ERROR, "Server sent an invalid mana-payment rejection.", false),
          );
        }
        break;
      }

      case "ManaPaymentPreviewFailed": {
        const data = msg.data as { request_id: number; message: string };
        const pending = this.pendingManaPaymentPreviews.get(data.request_id);
        if (pending?.socket === sourceSocket) {
          this.pendingManaPaymentPreviews.delete(data.request_id);
          pending.reject(new AdapterError(AdapterErrorCode.REQUEST_FAILED, data.message, true));
        }
        break;
      }

      case "InteractionPreview": {
        const data = msg.data as { preview: InteractionPreview };
        const pending = this.takeInteractionPreview(sourceSocket, data.preview.requestId);
        if (pending) {
          pending.resolve(data.preview);
        }
        break;
      }

      case "InteractionPreviewFailed": {
        const data = msg.data as { request_id: string; message: string };
        const pending = this.takeInteractionPreview(sourceSocket, data.request_id);
        if (pending) {
          pending.reject(new AdapterError(AdapterErrorCode.REQUEST_FAILED, data.message, true));
        }
        break;
      }

      case "GameOver": {
        const data = msg.data as { winner: PlayerId | null; reason: string };
        // Transition back to between_rounds — server auto-reports the
        // match result. Per T-59-09: adapter does NOT send ReportMatchResult.
        this.phase = "between_rounds";
        this.activeMatchId = null;
        this._gameCode = null;
        this.activeFullKey = null;
        this.acceptedFullKey = null;
        if (this.acceptedGameSocket === sourceSocket) this.acceptedGameSocket = null;
        this.clearSnapshot();
        this.emit({ type: "actionPendingChanged", pending: false });
        this.emit({
          type: "gameOver",
          winner: data.winner,
          reason: data.reason,
        });
        break;
      }

      case "OpponentDisconnected": {
        const data = msg.data as {
          grace_seconds: number;
          full_key?: FullSessionKey;
        };
        if (!fullSessionKeysEqual(data.full_key, this.acceptedFullKey)) {
          break;
        }
        this.emit({
          type: "opponentDisconnected",
          graceSeconds: data.grace_seconds,
        });
        break;
      }

      case "OpponentReconnected": {
        const data = (msg.data ?? {}) as { full_key?: FullSessionKey };
        if (fullSessionKeysEqual(data.full_key, this.acceptedFullKey)) {
          this.emit({ type: "opponentReconnected" });
        }
        break;
      }

      case "Pong": {
        // Silently consumed — latency tracking not needed for draft adapter.
        break;
      }

      case "Error": {
        const data = msg.data as { message: string };
        if (this.ws === sourceSocket) {
          this.settleInit(initOwner, new AdapterError("WS_ERROR", data.message, true));
        }
        this.emit({ type: "error", message: data.message });
        break;
      }
    }
  }

  /**
   * Maps DraftPlayerView.status to the adapter's internal phase.
   * Called after receiving DraftJoined and DraftStateUpdate.
   */
  private updatePhaseFromView(view: DraftPlayerView): void {
    switch (draftViewPresentation(view).podPhase) {
      case "lobby":
        this.phase = "lobby";
        break;
      case "drafting":
        this.phase = "drafting";
        break;
      case "review":
        this.phase = "review";
        break;
      case "deckbuilding":
        this.phase = "deckbuilding";
        break;
      case "pairing":
      case "roundComplete":
        this.phase = "between_rounds";
        break;
      case "matchInProgress":
        // Don't override "match" — DraftMatchStart sets it with game details.
        if (this.phase !== "match") {
          this.phase = "between_rounds";
        }
        break;
      case "complete":
      case "error":
        this.phase = "complete";
        break;
    }
  }

  // ── Reconnect ──────────────────────────────────────────────────────

  async reconnectDraft(): Promise<void> {
    const draftCode = this.draftCode;
    const playerToken = this.draftToken;
    if (!draftCode || !playerToken) {
      throw new AdapterError("WS_ERROR", "No draft session to reconnect to", false);
    }

    return new Promise<void>((resolve, reject) => {
      this.beginAttachment({ type: "init", init: { kind: "reconnect", draftCode, playerToken, resolve, reject } }, {
        type: "ReconnectDraft",
        data: {
          draft_code: draftCode,
          player_token: playerToken,
        },
      });
    });
  }

  // ── Utilities ──────────────────────────────────────────────────────

  private startPing(): void {
    if (this.pingInterval) {
      clearInterval(this.pingInterval);
    }
    this.pingInterval = setInterval(() => {
      this.send({ type: "Ping", data: { timestamp: Date.now() } });
    }, 5000);
  }

  private send(msg: unknown, ws: PhaseSocketTransport | null = this.ws): boolean {
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      this.emit({
        type: "error",
        message: "Cannot send message: WebSocket is not open.",
      });
      return false;
    }
    try {
      ws.send(JSON.stringify(msg));
      return true;
    } catch (err) {
      this.emit({
        type: "error",
        message: `Failed to send message: ${
          err instanceof Error ? err.message : String(err)
        }`,
      });
      return false;
    }
  }

  /** Rejects every pending preview, or only those sent on `onlySocket` when given. */
  private rejectPendingManaPaymentPreviews(error: Error, onlySocket?: PhaseSocketTransport): void {
    for (const [requestId, { socket, reject }] of this.pendingManaPaymentPreviews) {
      if (onlySocket && socket !== onlySocket) continue;
      this.pendingManaPaymentPreviews.delete(requestId);
      reject(error);
    }
  }

  private rejectPendingInteractionPreviews(error: Error, onlySocket?: PhaseSocketTransport): void {
    for (const [socket, previews] of this.pendingInteractionPreviews) {
      if (onlySocket && socket !== onlySocket) continue;
      this.pendingInteractionPreviews.delete(socket);
      for (const { reject } of previews.values()) reject(error);
    }
  }

  dispose(): void {
    this.disposed = true;
    if (this.pingInterval) {
      clearInterval(this.pingInterval);
      this.pingInterval = null;
    }
    // Publish disposal before closing any source; native close may synchronously reenter.
    this.ws = null;
    this.clearSnapshot();
    this._playerId = null;
    this._gameCode = null;
    this.draftCode = null;
    this.draftToken = null;
    this.draftView = null;
    this.seatIndex = null;
    this.activeMatchId = null;
    this.activeFullKey = null;
    this.acceptedFullKey = null;
    this.acceptedGameSocket = null;
    this.stateCodec.release();
    this.submission.drop()?.reject(
      new AdapterError("WS_CLOSED", "Adapter disposed during action", true),
    );
    this.submissionSocket = null;
    this.submissionFullKey = null;
    this.submissionSent = false;
    this.rejectPendingManaPaymentPreviews(
      new AdapterError("WS_CLOSED", "Adapter disposed during mana-payment preview", true),
    );
    this.rejectPendingInteractionPreviews(
      new AdapterError("WS_CLOSED", "Adapter disposed during interaction preview", true),
    );
    this.settleDraftAction(new AdapterError("WS_CLOSED", "Adapter disposed during draft operation", true));
    const disposed = new AdapterError("WS_CLOSED", "Adapter disposed before draft started", true);
    this.settleInit(this.pendingInit, disposed);
    this.cancelAttachment(disposed);
    for (const socket of this.sockets) this.releaseSocket(socket);
    this._serverInfo = null;
    this.emit({ type: "actionPendingChanged", pending: false });
    this.listeners = [];
  }
}
