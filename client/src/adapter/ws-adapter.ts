import type {
  EngineAdapter,
  EngineSnapshot,
  BatchResolveResult,
  GameAction,
  GameEvent,
  GameLogEntry,
  GameState,
  LegalActionsResult,
  MatchConfig,
  ManaCost,
  ObjectId,
  PlayerId,
  PersistedGameState,
  RewindOption,
  RewindTarget,
  SubmitResult,
  FormatConfig,
} from "./types";
import type { InteractionSubmission } from "./generated/interaction";
import { AdapterError, AdapterErrorCode, EMPTY_LEGAL_ACTIONS, actionRejectionError, nextSnapshotSeq, resolveAllRejectionError } from "./types";
import type { BracketDeckRequest, BracketEstimate } from "../types/bracketEstimate";
import {
  HandshakeError,
  openPhaseSocket,
  type PhaseSocket,
  type PhaseSocketFactory,
  type PhaseSocketTransport,
} from "../services/openPhaseSocket";
import { isValidWebSocketUrl, mixedContentBlockReason } from "../services/serverDetection";
import type { FullSessionKey, WsSessionData } from "../services/multiplayerSession";
import {
  commitFullTerminalDelivery,
  type FullTerminalDelivery,
} from "../services/fullTerminalResult";

/** Deck data format matching server protocol. */
export interface DeckData {
  main_deck: string[];
  sideboard: string[];
  commander?: string[];
  companion?: string[];
  signature_spell?: string[];
  planar_deck?: string[];
  scheme_deck?: string[];
  sticker_sheets?: string[];
}

export type { FullSessionKey } from "../services/multiplayerSession";

/**
 * Performs one terminal-only request on a fresh raw websocket. This never
 * constructs a WebSocketAdapter, so a recovered terminal cannot initialize a
 * game loop or acquire a normal Full attachment.
 */
async function terminalSocketRequest(
  serverUrl: string,
  request: unknown,
  socketFactory?: PhaseSocketFactory,
): Promise<{ type: string; data?: unknown }> {
  const socket = await openPhaseSocket(serverUrl, { socketFactory });
  return new Promise((resolve, reject) => {
    const closeAndResolve = (message: { type: string; data?: unknown }) => {
      socket.ws.close();
      resolve(message);
    };
    socket.ws.onmessage = (event) => {
      try {
        closeAndResolve(JSON.parse(event.data as string) as { type: string; data?: unknown });
      } catch (error) {
        socket.ws.close();
        reject(error);
      }
    };
    socket.ws.onerror = () => {
      socket.ws.close();
      reject(new Error("Terminal websocket request failed"));
    };
    socket.ws.onclose = () => {
      reject(new Error("Terminal websocket closed before a response"));
    };
    socket.ws.send(JSON.stringify(request));
  });
}

export async function bootstrapFullTerminalDelivery(
  serverUrl: string,
  key: FullSessionKey,
  playerToken: string,
  requestId: string,
  socketFactory?: PhaseSocketFactory,
): Promise<FullTerminalDelivery | null> {
  const response = await terminalSocketRequest(
    serverUrl,
    {
      type: "BootstrapTerminalDelivery",
      data: {
        request: {
          key,
          playerToken,
          requestId,
        },
      },
    },
    socketFactory,
  );
  if (response.type !== "TerminalBootstrapResult") {
    throw new Error("Unexpected terminal bootstrap response");
  }
  return (response.data as { delivery?: FullTerminalDelivery }).delivery ?? null;
}

export async function readFullTerminalResult(
  serverUrl: string,
  credential: string,
  socketFactory?: PhaseSocketFactory,
): Promise<FullTerminalDelivery | null> {
  const response = await terminalSocketRequest(
    serverUrl,
    { type: "ReadTerminalResult", data: { credential } },
    socketFactory,
  );
  if (response.type !== "TerminalResult") {
    throw new Error("Unexpected terminal result response");
  }
  return (response.data as { delivery?: FullTerminalDelivery }).delivery ?? null;
}

export async function acknowledgeFullTerminalDelivery(
  serverUrl: string,
  deliveryId: string,
  credential: string,
  socketFactory?: PhaseSocketFactory,
): Promise<boolean> {
  const response = await terminalSocketRequest(
    serverUrl,
    { type: "AckTerminalDelivery", data: { delivery_id: deliveryId, credential } },
    socketFactory,
  );
  return response.type === "TerminalDeliveryAcknowledged";
}

/** AI seat configuration for the private native-engine host path. */
export interface NativeAiSeat {
  seatIndex: number;
  difficulty: string;
  deck: DeckData;
}

/**
 * Native single-player configuration. This stays deliberately separate from
 * lobby hosting: the native server receives a private, all-AI game request and
 * never registers a public room or emits multiplayer-store session state.
 */
export interface NativeAiAdapterOptions {
  socketFactory: PhaseSocketFactory;
  aiSeats: NativeAiSeat[];
  playerCount: number;
  formatConfig?: FormatConfig;
  matchConfig?: MatchConfig;
  /** Present on release only; preview parity is verified by the shell. */
  expectedServerVersion?: string;
}

/** Transport contract shared by the native single-player and P2P-host paths. */
export interface NativeSocketAdapterOptions {
  socketFactory: PhaseSocketFactory;
  /** Present on release only; preview parity is verified by the shell. */
  expectedServerVersion?: string;
}

/** Native server setup for one local P2P seat. The PeerJS connection remains
 * the guest-facing transport; these sockets never leave the desktop host. */
export type NativePregameAdapterOptions =
  | ({ kind: "host"; aiSeats: NativeAiSeat[]; playerCount: number; formatConfig?: FormatConfig; matchConfig?: MatchConfig } & NativeSocketAdapterOptions)
  | ({ kind: "guest" } & NativeSocketAdapterOptions)
  | ({ kind: "reconnect"; gameCode: string; playerId: PlayerId; playerToken: string; fullKey: FullSessionKey } & NativeSocketAdapterOptions);

export interface NativeSessionAttachment {
  gameCode: string;
  playerId: PlayerId;
  playerToken: string;
  fullKey: FullSessionKey;
}

export interface WebSocketAdapterOptions {
  nativeAi?: NativeAiAdapterOptions;
  nativePregame?: NativePregameAdapterOptions;
}

export class NativeEngineVersionMismatchError extends Error {
  constructor(
    public readonly expected: string,
    public readonly actual: string,
  ) {
    super("Native engine version does not match this release");
    this.name = "NativeEngineVersionMismatchError";
  }
}

/**
 * Wire-protocol version the client speaks. Must match `PROTOCOL_VERSION` in
 * `crates/server-core/src/protocol.rs`. Bump in lockstep when either side
 * adds, removes, renames, or changes the type of a protocol variant field.
 *
 * 32 — DerivedViews.legend_candidate_identities publishes the engine-authored
 *      original/copy/token-copy identity for each active legend-rule choice. The
 *      field is serde-optional, but the client deliberately no longer derives this
 *      rules-sensitive identity from raw objects; an older server would silently
 *      omit every choice identity.
 * 31 — WaitingFor::LoopShortcut publishes the engine-issued declaration, and
 *      InteractionResponseSpec::Shortcut publishes preview, the per-axis
 *      consequence of the offered count. Both are optional and neither type
 *      sets deny_unknown_fields, so a v30 peer still PARSES the frame — a
 *      capability bump like 24, not a parse bump. UNLIKE 24, no pairing is left
 *      for the capability gap to bite in, so this entry names no silent-drop
 *      hazard: full-game floors are exact-match on BOTH sides
 *      (MIN_SUPPORTED_SERVER_PROTOCOL below, and MIN_SUPPORTED_PROTOCOL in
 *      crates/server-core/src/protocol.rs, each equal to their own
 *      PROTOCOL_VERSION), so a v31/v30 full-game pair is refused at the
 *      handshake and never sends an action frame. The one-version window that
 *      does exist is lobby-only (LOBBY_MIN_SUPPORTED_SERVER_PROTOCOL below /
 *      MIN_SUPPORTED_PROTOCOL in crates/lobby-broker/src/protocol.rs) and it
 *      cannot carry this capability either: DeclareShortcut rides
 *      ClientMessage::Action, which LobbyClientMessage has no variant for at
 *      all, and which reject_if_disabled in crates/phase-server/src/main.rs
 *      answers under ServerMode::LobbyOnly with an explicit rejection rather
 *      than a silent drop.
 * 30 — Serialized player-action completion provenance and modal continuations.
 * 29 — Added requester-correlated ResolveAllRejected response frames.
 * 28 — Added native ResolveAll request/result frames.
 * 27 — Added DraftKind.Sealed, serialized by draft WebSocket messages.
 * 26 — Added ActionNoOp acknowledgement for accepted transport no-ops.
 * 25 — DebugCardEntries added a serialized, private resolution frame for
 *      multi-card sandbox battlefield entries that pause for replacement or
 *      as-enters choices. Old peers cannot deserialize that GameState shape.
 * 24 — DerivedViews.unbounded_families carries the engine-owned per-seat family
 *      collapse state behind each ∞ badge. A CAPABILITY bump, not a parse bump:
 *      the field is serde-optional, but this client deleted its row-flag
 *      OR-fold derivation, so a v23 server that omits the field would leave
 *      this client rendering NO infinity badges — silently, with no parse error
 *      to catch it. The handshake is the only place that pairing is refusable.
 * 23 — PayableResource::ManaGeneric changed from { per_x } to
 *      { base_cost: ManaCost } (#6410) — a GameState payload field type
 *      change, and base_cost intentionally carries no serde default (a
 *      missing base_cost must fail deserialization, not silently resolve
 *      to a zero-cost payment), so old and new peers can't parse each
 *      other's serialized state.
 * 22 — Viewer interaction projections and semantic object-action identities.
 * 21 — Native P2P host bridge identity and server-authored state revisions.
 * 20 — Actor-scoped priority-passing settings and filtered per-player state.
 * 19 — Connive exact subject snapshots and resident paused post-replacement
 *      drains changed the serialized full-game state. Phase 4 later pinned
 *      the existing v2 resolution wire shape without another protocol change.
 * 17 — Dedicated companion deck slot and typed companion-reveal choices.
 * 16 — Meld pair/attacking-entry choices after the mana-payment preview variants.
 * 15 — Mana-payment preview request/response variants.
 * 14 — PrecastCopyShortcut action and its two WaitingFor variants.
 * 13 — WaitingFor::MulliganBottomCards removed; mulligan bottoming folded
 *      into a MulliganDecisionPhase::BottomCards sub-phase on
 *      WaitingFor::MulliganDecision.
 */
export const PROTOCOL_VERSION = 32;

/**
 * Lowest server protocol version this client will accept in the handshake.
 * Planechase changed the wire message surface in a non-backward-compatible way,
 * so this release only accepts the current protocol.
 */
export const MIN_SUPPORTED_SERVER_PROTOCOL = PROTOCOL_VERSION;

/**
 * Lowest server protocol version this client accepts for lobby-only brokers.
 * LobbyOnly carries matchmaking metadata only, so it keeps a one-version
 * rollout window while Full servers stay current-only.
 */
export const LOBBY_MIN_SUPPORTED_SERVER_PROTOCOL = PROTOCOL_VERSION - 1;

/** Identity advertised by the server in its `ServerHello`. */
export interface ServerInfo {
  version: string;
  buildCommit: string;
  protocolVersion: number;
  mode: "Full" | "LobbyOnly";
  /** Public base URL the server advertises for `<code>@<host>` join strings
   * (a tunnel/proxy URL), or undefined when the server has none to share. */
  publicUrl?: string;
}

/** Events emitted by the WebSocketAdapter for UI state updates. */
export type WsAdapterEvent =
  | { type: "serverHello"; info: ServerInfo; compatible: boolean }
  | { type: "playerIdentity"; playerId: PlayerId; opponentName: string | null; playerNames?: Record<number, string> }
  | { type: "actionPendingChanged"; pending: boolean }
  | { type: "latencyChanged"; latencyMs: number | null }
  | { type: "sessionChanged"; session: WsSessionData | null }
  | { type: "gameCreated"; gameCode: string }
  | { type: "passwordRequired"; gameCode: string }
  | { type: "waitingForOpponent" }
  | { type: "opponentJoined"; opponentName?: string }
  | { type: "opponentDisconnected"; graceSeconds: number }
  | { type: "opponentReconnected" }
  | { type: "playerDisconnected"; playerId: PlayerId; graceSeconds: number }
  | { type: "playerReconnected"; playerId: PlayerId }
  | { type: "gamePaused"; disconnectedPlayer: PlayerId; timeoutSeconds: number }
  | { type: "gameResumed" }
  | { type: "playerEliminated"; playerId: PlayerId; becameSpectator: boolean }
  | { type: "spectatorJoined"; name: string }
  | { type: "gameOver"; winner: PlayerId | null; reason: string }
  | { type: "error"; message: string }
  | { type: "deckRejected"; reason: string }
  | { type: "reconnecting"; attempt: number; maxAttempts: number }
  | { type: "reconnected" }
  | { type: "reconnectFailed" }
  | { type: "terminalDelivery"; delivery: FullTerminalDelivery }
  | { type: "terminalUnavailable"; message: string }
  /** The engine pair travels as one `EngineSnapshot` — see the P2P adapter's
   *  `stateChanged` for why the halves must stay inseparable. */
  | { type: "stateChanged"; snapshot: EngineSnapshot; events: GameEvent[]; logEntries?: GameLogEntry[]; serverRevision?: number;
      /** Server-published turn boundaries. Always an array on this transport —
       *  `[]` means "the server published none", never "unknown". */
      rewindTargets?: RewindOption[] }
  | { type: "sessionAttached"; attachment: NativeSessionAttachment }
  | { type: "emoteReceived"; fromPlayer: PlayerId; emote: string }
  | { type: "conceded"; player: PlayerId }
  | { type: "timerUpdate"; player: PlayerId; remainingSeconds: number }
  | { type: "takebackRequested"; requester: PlayerId; requesterName: string }
  | { type: "takebackResolved"; approved: boolean; resolvedBy: PlayerId | null }
  /** The server refused a fire-and-forget request (e.g. `RequestTakeback`).
   *  Distinct from `error`, which the native session treats as terminal:
   *  this one is survivable and carries a server-authored reason to show. */
  | { type: "requestRejected"; reason: string };

type WsAdapterEventListener = (event: WsAdapterEvent) => void;

function playerNamesFromWire(names: string[]): Record<number, string> {
  const playerNames: Record<number, string> = {};
  names.forEach((name, playerId) => {
    if (name.length > 0) {
      playerNames[playerId] = name;
    }
  });
  return playerNames;
}

/**
 * WebSocket-backed implementation of EngineAdapter.
 * Communicates with the phase-server via WebSocket protocol
 * for multiplayer games.
 */
export class WebSocketAdapter implements EngineAdapter {
  readonly supportsMatchConcede = true;
  readonly supportsServerRewind = true;
  readonly resolveAllUsesServerAi: true | undefined;
  private ws: PhaseSocketTransport | null = null;
  /**
   * The single cached engine pair, rebuilt (and re-stamped) once per inbound
   * state-bearing message. `getState`/`getLegalActions` both read from THIS
   * object, so they can no longer straddle two updates. The WebSocket delivers
   * server messages in order, so stamping on arrival reproduces engine order.
   */
  private snapshot: EngineSnapshot | null = null;
  private _playerId: PlayerId | null = null;
  private playerToken: string | null = null;
  private _gameCode: string | null = null;
  private fullSessionKey: FullSessionKey | null = null;
  private pendingResolve: ((result: SubmitResult) => void) | null = null;
  private pendingReject: ((error: Error) => void) | null = null;
  private nextResolveAllRequestId = 1;
  private pendingResolveAll: {
    requestId: number;
    resolve: (result: BatchResolveResult) => void;
    reject: (error: Error) => void;
  } | null = null;
  private nextManaPaymentPreviewRequestId = 1;
  private pendingManaPaymentPreviews = new Map<
    number,
    { resolve: (sourceIds: ObjectId[]) => void; reject: (error: Error) => void }
  >();
  private initResolve: (() => void) | null = null;
  private initReject: ((error: Error) => void) | null = null;
  /** Starting-player contest event captured from the initial GameStarted
   *  message, handed back by `initializeGame()` so the dice overlay animates it.
   *  Empty on reconnects (the server drains it after first send). */
  private initStartEvents: GameEvent[] = [];
  private pregameResolve: ((attachment: NativeSessionAttachment) => void) | null = null;
  private pregameReject: ((error: Error) => void) | null = null;
  private gameStartedResolve: (() => void) | null = null;
  private gameStartedReject: ((error: Error) => void) | null = null;
  private receivedGameStarted = false;
  private pregameMutationResolve: (() => void) | null = null;
  private pregameMutationReject: ((error: Error) => void) | null = null;
  private pregameMutationSlotsRevision: number | null = null;
  private playerSlotsRevision = 0;
  private playerSlotsResolve: (() => void) | null = null;
  private playerSlotsReject: ((error: Error) => void) | null = null;
  private playerSlotsTargetRevision: number | null = null;
  private abandonResolve: (() => void) | null = null;
  private abandonReject: ((error: Error) => void) | null = null;
  private listeners: WsAdapterEventListener[] = [];
  private reconnectAttempt = 0;
  // A native bridge has no resumable server session: a dead loopback engine
  // cannot recover through the multiplayer reconnect protocol.
  private readonly maxReconnectAttempts: number;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private pingInterval: ReturnType<typeof setInterval> | null = null;
  private disposed = false;
  /** A rejected Full identity is terminal for this socket. */
  private sessionIdentityRejected = false;
  private gameEnded = false;
  /**
   * Populated once the server's `ServerHello` arrives. `null` between the
   * WebSocket opening and the hello being delivered. Consumers see it via
   * the `serverHello` event, or through `getServerInfo()`.
   */
  private _serverInfo: ServerInfo | null = null;
  /**
   * `true` when we're inside a `tryReconnect` flow. Used by the `GameStarted`
   * path in `handleMessage` to emit a `reconnected` event exactly once when
   * the server confirms the resumed session.
   */
  private reconnectInFlight = false;
  /**
   * `true` between `GameCreated` (host path) and the first `GameStarted`.
   * When `GameStarted` arrives with this flag set, emit `opponentJoined`
   * exactly once so the UI can fire a browser notification. Cleared on
   * first fire so re-connects and state updates don't re-notify.
   */
  private hostWaitingForOpponent = false;

  constructor(
    private readonly serverUrl: string,
    private readonly mode: "host" | "join" | "spectate",
    private readonly deckData: DeckData,
    private readonly joinGameCode?: string,
    private readonly joinPassword?: string,
    private readonly reservationToken?: string,
    private readonly displayName = "Player",
    private readonly options: WebSocketAdapterOptions = {},
  ) {
    this.resolveAllUsesServerAi = options.nativeAi ? true : undefined;
    // 0 is terminal, not "retry once": `attemptReconnect` compares
    // `reconnectAttempt >= maxReconnectAttempts`, so 0 >= 0 is true on the
    // very first attempt — it emits `reconnectFailed` and returns without
    // ever emitting `reconnecting`, incrementing, or scheduling a timer. Any
    // socket drop is instantly fatal.
    //
    // `nativeAi` no longer takes that. The sidecar runs `--single-user`, so
    // its reconnect grace period is effectively unbounded and sessions are
    // never stale-purged; a transient drop against a live sidecar is exactly
    // the recoverable case, and 8 attempts is the same budget `online` gets.
    //
    // Not purely additive: a genuinely DEAD sidecar now spends **32s** of
    // `reconnecting` UI before `reconnectFailed`, where today it fails
    // instantly. `attemptReconnect`'s
    // `Math.min(Math.pow(2, attempt - 1) * 1000, 5000)` over attempts 1-8 is
    // 1+2+4+5+5+5+5+5 = 32, not the 27 first claimed here — the series was
    // right and the sum was wrong.
    //
    // That is a floor, not a ceiling. It assumes each connect is *refused*
    // immediately. If they hang instead, `attachSocket` calls
    // `openPhaseSocket` without a `timeoutMs`, taking its 5000ms default, so
    // the worst case is 32 + 8x5 = 72s.
    //
    // Still the right trade — the common case goes from fatal to recovered —
    // but it is a user-visible latency regression on the most likely desktop
    // failure, and for a LOOPBACK socket that failure is a crash or a
    // sleep/resume rather than a spurious drop. 8 attempts buys little over
    // 3-4 there, and nothing respawns the sidecar: `ensureNativeEngine` is
    // never called from a close or error handler. Worth revisiting; the
    // budget is deliberately left matching `online` rather than tuned here.
    //
    // `nativePregame` keeps 0, and that is out of scope rather than endorsed:
    // a pregame drop is recoverable by re-entering the lobby (no game has
    // been invested yet), and that path has adapter-level special-casing of
    // `options.nativePregame` that has NOT been analysed here.
    this.maxReconnectAttempts = options.nativePregame ? 0 : 8;
  }

  get gameCode(): string | null {
    return this._gameCode;
  }

  get playerId(): PlayerId | null {
    return this._playerId;
  }

  /** Reconnect credentials for this session, or null until the server has
   *  assigned them (i.e. game creation has completed). Solo-AI native games
   *  persist this so a suspended game can be resumed by constructing a
   *  `kind: "reconnect"` adapter. The player token is issued once at creation
   *  and lives only client-side — it is the reconnect security boundary. */
  get nativeSession(): { gameCode: string; playerId: PlayerId; playerToken: string; fullKey: FullSessionKey } | null {
    if (this._gameCode === null || this._playerId === null || this.playerToken === null || this.fullSessionKey === null) {
      return null;
    }
    return {
      gameCode: this._gameCode,
      playerId: this._playerId,
      playerToken: this.playerToken,
      fullKey: this.fullSessionKey,
    };
  }

  onEvent(listener: WsAdapterEventListener): () => void {
    this.listeners.push(listener);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener);
    };
  }

  private emit(event: WsAdapterEvent): void {
    for (const listener of this.listeners) {
      listener(event);
    }
  }

  async initializeGame(
    _deckData?: unknown,
    _formatConfig?: unknown,
    _playerCount?: number,
    _matchConfig?: unknown,
    _firstPlayer?: number,
  ): Promise<SubmitResult> {
    // Server handles deck data via WebSocket protocol during initialize().
    // The starting-player contest events (if any) were captured from the
    // initial GameStarted message; hand them back so gameStore.initGame routes
    // them to the dice overlay, then clear so they're consumed once.
    const events = this.initStartEvents;
    this.initStartEvents = [];
    return { events };
  }

  async initialize(): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      this.initResolve = resolve;
      this.initReject = reject;

      if (!this.isNativeSocket() && !isValidWebSocketUrl(this.serverUrl)) {
        reject(new AdapterError("WS_ERROR", "Invalid WebSocket URL", false));
        this.initResolve = null;
        this.initReject = null;
        return;
      }

      // A ws:// target from an HTTPS page is blocked by the browser before the
      // handshake — surface why instead of letting it fail as "unreachable".
      const blockReason = this.isNativeSocket()
        ? null
        : mixedContentBlockReason(this.serverUrl);
      if (blockReason) {
        reject(new AdapterError("WS_ERROR", blockReason, false));
        this.initResolve = null;
        this.initReject = null;
        return;
      }

      this.seedNativeReconnectSession();
      const setupFrame =
        this.options.nativeAi
          ? this.nativeAiSetupFrame(this.options.nativeAi)
          : this.options.nativePregame
            ? this.nativePregameSetupFrame(this.options.nativePregame)
          : this.mode === "host"
          ? { type: "CreateGame", data: { deck: this.deckData } }
          : this.mode === "spectate"
            ? { type: "SpectatorJoin", data: { game_code: this.joinGameCode! } }
            : {
                type: "JoinGameWithPassword",
                data: {
                  game_code: this.joinGameCode!,
                  deck: this.deckData,
                  display_name: this.displayName,
                  password: this.joinPassword ?? null,
                  reservation_token: this.reservationToken ?? null,
                },
              };

      this.attachSocket(setupFrame).catch(() => {
        // `attachSocket` emits reject via initReject; swallow the
        // rejection here so it doesn't surface as an unhandled promise.
      });
    });
  }

  /** Connect to a local native full server and stop once this socket has a
   * server-issued pregame seat identity. `initialize()` intentionally remains
   * game-start based for normal server sessions. */
  async initializePregame(): Promise<NativeSessionAttachment> {
    const options = this.options.nativePregame;
    if (!options) {
      throw new AdapterError("WS_ERROR", "Pregame initialization requires a native socket", false);
    }
    this.seedNativeReconnectSession();
    return new Promise<NativeSessionAttachment>((resolve, reject) => {
      this.pregameResolve = resolve;
      this.pregameReject = reject;
      this.attachSocket(this.nativePregameSetupFrame(options)).catch(() => {
        // attachSocket settles the pending lifecycle promise.
      });
    });
  }

  /** Resolves once the server has started this pregame session. */
  async waitForGameStarted(): Promise<void> {
    if (this.receivedGameStarted) return;
    return new Promise<void>((resolve, reject) => {
      this.gameStartedResolve = resolve;
      this.gameStartedReject = reject;
    });
  }

  async sendSeatMutation(mutation: unknown): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      this.pregameMutationResolve = resolve;
      this.pregameMutationReject = reject;
      this.pregameMutationSlotsRevision = this.playerSlotsRevision;
      if (!this.send({ type: "SeatMutate", data: { mutation } })) {
        this.pregameMutationResolve = null;
        this.pregameMutationReject = null;
        this.pregameMutationSlotsRevision = null;
        reject(new AdapterError("WS_CLOSED", "Failed to send seat mutation", true));
      }
    });
  }

  /** Wait for the next authoritative pregame-slot broadcast. Native bridge
   * orchestration uses this to serialize host edits and guest attachment. */
  async waitForPlayerSlots(): Promise<void> {
    const targetRevision = this.playerSlotsRevision + 1;
    return new Promise<void>((resolve, reject) => {
      this.playerSlotsResolve = resolve;
      this.playerSlotsReject = reject;
      this.playerSlotsTargetRevision = targetRevision;
    });
  }

  async sendAbandonGame(): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      this.abandonResolve = resolve;
      this.abandonReject = reject;
      if (!this.send({ type: "AbandonGame" })) {
        this.abandonResolve = null;
        this.abandonReject = null;
        reject(new AdapterError("WS_CLOSED", "Failed to abandon native game", true));
      }
    });
  }

  /**
   * Opens a `PhaseSocket` via the shared handshake helper, caches the
   * `ServerInfo`, wires the post-handshake message/close handlers, and
   * sends `setupFrame`. Used by both `initialize()` and `tryReconnect()`
   * so the handshake policy lives in exactly one place.
   */
  private async attachSocket(setupFrame: unknown): Promise<void> {
    let socket: PhaseSocket<PhaseSocketTransport>;
    try {
      socket = await openPhaseSocket(this.serverUrl, {
        socketFactory: this.nativeSocketOptions()?.socketFactory,
      });
    } catch (err) {
      if (err instanceof HandshakeError) {
        const retryable = err.kind !== "protocol_mismatch" && err.kind !== "invalid_url";
        const adapterErr = new AdapterError("WS_ERROR", err.message, retryable);
        this.rejectInitialization(adapterErr);
        if (err.kind === "protocol_mismatch" && err.serverInfo) {
          // Incompatible handshake — surface an explicit event so the
          // UI can render the version-mismatch prompt even if no one is
          // awaiting `initialize()`. Use the real `ServerInfo` parsed
          // from `ServerHello` so the UI can render accurate
          // "server is on X, you are on Y" diagnostics.
          this._serverInfo = err.serverInfo;
          this.emit({
            type: "serverHello",
            info: err.serverInfo,
            compatible: false,
          });
        }
        return;
      }
      this.rejectInitialization(new AdapterError("WS_ERROR", String(err), true));
      return;
    }

    if (
      this.nativeSocketOptions()?.expectedServerVersion !== undefined
      && socket.serverInfo.version !== this.nativeSocketOptions()!.expectedServerVersion
    ) {
      socket.close();
      const error = new NativeEngineVersionMismatchError(
        this.nativeSocketOptions()!.expectedServerVersion!,
        socket.serverInfo.version,
      );
      this.rejectInitialization(error);
      return;
    }

    this.ws = socket.ws;
    this._serverInfo = socket.serverInfo;
    this.emit({ type: "serverHello", info: socket.serverInfo, compatible: true });
    this.startPing();

    socket.ws.onmessage = (event) => {
      this.handleMessage(JSON.parse(event.data as string));
    };

    socket.ws.onerror = () => {
      const err = new AdapterError("WS_ERROR", "WebSocket connection failed", true);
      if (this.initReject || this.pregameReject || this.gameStartedReject) {
        this.rejectInitialization(err);
      } else {
        this.emit({ type: "error", message: err.message });
      }
    };

    socket.ws.onclose = () => {
      if (this.pingInterval) {
        clearInterval(this.pingInterval);
        this.pingInterval = null;
      }
      if (this.sessionIdentityRejected) return;
      // Clear the "host waiting for opponent" latch on socket close —
      // otherwise a host who received GameCreated, disconnected before
      // GameStarted, and then reconnected through a different path would
      // fire `opponentJoined` spuriously on the replayed GameStarted.
      this.hostWaitingForOpponent = false;
      if (this.pendingReject) {
        this.emit({ type: "actionPendingChanged", pending: false });
        this.pendingReject(
          new AdapterError("WS_CLOSED", "Connection closed during action", true),
        );
        this.pendingResolve = null;
        this.pendingReject = null;
      }
      if (this.pendingResolveAll) {
        this.pendingResolveAll.reject(
          new AdapterError("WS_CLOSED", "Connection closed during Resolve All", true),
        );
        this.pendingResolveAll = null;
      }
      this.rejectPendingManaPaymentPreviews(
        new AdapterError("WS_CLOSED", "Connection closed during mana-payment preview", true),
      );
      this.rejectPregameMutation(
        new AdapterError("WS_CLOSED", "Connection closed during seat mutation", true),
      );
      this.rejectAbandon(new AdapterError("WS_CLOSED", "Connection closed while abandoning game", true));
      if (this.initReject) {
        this.initReject(
          new AdapterError("WS_CLOSED", "Connection closed before game started", true),
        );
        this.initResolve = null;
        this.initReject = null;
      } else if (this.pregameReject) {
        this.pregameReject(
          new AdapterError("WS_CLOSED", "Connection closed before native seat attachment", true),
        );
        this.pregameResolve = null;
        this.pregameReject = null;
      } else if (this.gameStartedReject) {
        this.gameStartedReject(
          new AdapterError("WS_CLOSED", "Connection closed before game started", true),
        );
        this.gameStartedResolve = null;
        this.gameStartedReject = null;
      } else if (this.snapshot !== null || this.playerToken !== null) {
        this.attemptReconnect();
      }
    };

    if (!this.send(setupFrame)) {
      socket.close();
      if (this.initReject) {
        this.initReject(
          new AdapterError("WS_CLOSED", "Failed to send setup frame", true),
        );
        this.initResolve = null;
        this.initReject = null;
      }
    }
  }

  async submitAction(action: GameAction, _actor: PlayerId): Promise<SubmitResult> {
    // `_actor` is the local player's PlayerId. The WebSocket wire format
    // intentionally omits it — the server derives the authoritative actor
    // from the join-token-authenticated session, never from the payload.
    // A client-supplied actor here would provide zero additional safety and
    // only creates a spoofing surface if it were ever put on the wire.
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new AdapterError("WS_ERROR", "WebSocket not connected", false);
    }

    this.emit({ type: "actionPendingChanged", pending: true });
    return new Promise<SubmitResult>((resolve, reject) => {
      this.pendingResolve = resolve;
      this.pendingReject = reject;
      // If the frame cannot be sent, the server will never reply, so clear the
      // pending state and reject now instead of leaving the caller hanging.
      if (!this.send({ type: "Action", data: { action } })) {
        this.pendingResolve = null;
        this.pendingReject = null;
        this.emit({ type: "actionPendingChanged", pending: false });
        reject(new AdapterError("WS_CLOSED", "Failed to send action", true));
      }
    });
  }

  async submitInteraction(
    submission: InteractionSubmission,
    _actor: PlayerId,
  ): Promise<SubmitResult> {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new AdapterError("WS_ERROR", "WebSocket not connected", false);
    }

    this.emit({ type: "actionPendingChanged", pending: true });
    return new Promise<SubmitResult>((resolve, reject) => {
      this.pendingResolve = resolve;
      this.pendingReject = reject;
      if (!this.send({ type: "Interaction", data: { submission } })) {
        this.pendingResolve = null;
        this.pendingReject = null;
        this.emit({ type: "actionPendingChanged", pending: false });
        reject(new AdapterError("WS_CLOSED", "Failed to send interaction", true));
      }
    });
  }

  async resolveAll(
    _requester: PlayerId,
    _aiSeats: { playerId: number; difficulty: string }[],
    maxResolutions = 5_000,
  ): Promise<BatchResolveResult> {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new AdapterError("WS_ERROR", "WebSocket not connected", false);
    }
    if (this.pendingResolveAll) {
      throw new AdapterError("WS_ERROR", "Resolve All already pending", false);
    }

    const requestId = this.nextResolveAllRequestId++;
    return new Promise<BatchResolveResult>((resolve, reject) => {
      this.pendingResolveAll = { requestId, resolve, reject };
      if (!this.send({
        type: "ResolveAll",
        data: { request_id: requestId, max_resolutions: maxResolutions },
      })) {
        this.pendingResolveAll = null;
        reject(new AdapterError("WS_CLOSED", "Failed to send Resolve All", true));
      }
    });
  }

  async previewManaPayment(action: GameAction, _actor: PlayerId): Promise<ObjectId[]> {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new AdapterError("WS_ERROR", "WebSocket not connected", false);
    }

    const requestId = this.nextManaPaymentPreviewRequestId++;
    return new Promise<ObjectId[]>((resolve, reject) => {
      this.pendingManaPaymentPreviews.set(requestId, { resolve, reject });
      if (!this.send({ type: "PreviewManaPayment", data: { request_id: requestId, action } })) {
        this.pendingManaPaymentPreviews.delete(requestId);
        reject(new AdapterError("WS_CLOSED", "Failed to send mana-payment preview", true));
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
  private cacheSnapshot(state: GameState, legalResult: LegalActionsResult): EngineSnapshot {
    this.snapshot = { state, legalResult, seq: nextSnapshotSeq() };
    return this.snapshot;
  }

  restoreState(_state: PersistedGameState): void {
    throw new AdapterError(
      AdapterErrorCode.WASM_ERROR,
      "Undo not supported in multiplayer",
      false,
    );
  }

  estimateBracket(_deck: BracketDeckRequest): Promise<BracketEstimate | null> {
    throw new AdapterError(
      AdapterErrorCode.BRACKET_ESTIMATION_UNSUPPORTED,
      "Bracket estimation is a local feature; not available in WebSocket sessions.",
      false,
    );
  }

  sendConcede(): void {
    this.send({ type: "Concede" });
  }

  /** Requests a whole-match concession for this authenticated session. */
  sendMatchConcede(): void {
    this.send({ type: "ConcedeMatch" });
  }

  sendEmote(emote: string): void {
    this.send({ type: "Emote", data: { emote } });
  }

  /**
   * GH #1507: ask every other human player to approve rolling the game back.
   * Defaults to the pre-existing last-action granularity, which keeps the
   * existing zero-argument call site behaving identically.
   *
   * The last-action frame deliberately carries NO `data` key — byte-identical
   * to the frame this client has always sent, and the shape the server's
   * `Option<RewindTarget>` newtype variant exists to accept. Only a turn rewind
   * carries a payload, and the client can only ask for a turn the server itself
   * published in `rewind_targets`.
   */
  sendRequestTakeback(target: RewindTarget = { kind: "last_action" }): void {
    if (target.kind === "last_action") {
      this.send({ type: "RequestTakeback" });
      return;
    }
    this.send({ type: "RequestTakeback", data: target });
  }

  /** Approve or decline a pending takeback request. */
  sendRespondTakeback(approve: boolean): void {
    this.send({ type: "RespondTakeback", data: { approve } });
  }

  /** Withdraw a takeback request this player made themselves. */
  sendCancelTakeback(): void {
    this.send({ type: "CancelTakeback" });
  }

  sendReadyToggle(): void {
    this.send({ type: "ReadyToggle" });
  }

  sendSpectatorJoin(gameCode: string): void {
    this.send({ type: "SpectatorJoin", data: { game_code: gameCode } });
  }

  sendStartGame(): void {
    this.send({ type: "StartGame" });
  }

  dispose(options?: { concede?: boolean }): void {
    if (options?.concede && !this.gameEnded) {
      this.sendConcede();
    }
    this.disposed = true;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.pingInterval) {
      clearInterval(this.pingInterval);
      this.pingInterval = null;
    }
    this.rejectInitialization(
      new AdapterError("WS_CLOSED", "Adapter disposed before initialization completed", true),
    );
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
    this.snapshot = null;
    this._playerId = null;
    this.playerToken = null;
    this._gameCode = null;
    this.fullSessionKey = null;
    this.pendingResolve = null;
    this.pendingReject = null;
    if (this.pendingResolveAll) {
      this.pendingResolveAll.reject(
        new AdapterError("WS_CLOSED", "Adapter disposed during Resolve All", true),
      );
      this.pendingResolveAll = null;
    }
    this.rejectPendingManaPaymentPreviews(
      new AdapterError("WS_CLOSED", "Adapter disposed during mana-payment preview", true),
    );
    this.rejectPregameMutation(
      new AdapterError("WS_CLOSED", "Adapter disposed during seat mutation", true),
    );
    this.rejectAbandon(new AdapterError("WS_CLOSED", "Adapter disposed while abandoning game", true));
    this.reconnectInFlight = false;
    this._serverInfo = null;
    this.receivedGameStarted = false;
    this.emit({ type: "actionPendingChanged", pending: false });
    this.emit({ type: "latencyChanged", latencyMs: null });
    if (this.gameEnded) {
      this.emit({ type: "sessionChanged", session: null });
    }
    this.listeners = [];
  }

  /** Attempt reconnection using stored session data. */
  tryReconnect(session: WsSessionData): boolean {
    this._gameCode = session.gameCode;
    this.playerToken = session.playerToken;
    this.fullSessionKey = session.fullKey;

    if (!this.isNativeSocket() && !isValidWebSocketUrl(this.serverUrl)) {
      this.emit({ type: "reconnectFailed" });
      return false;
    }

    this.reconnectInFlight = true;
    this.attachSocket({
      type: "Reconnect",
      data: {
        game_code: session.gameCode,
        player_token: session.playerToken,
        full_key: session.fullKey,
      },
    }).catch(() => {
      // attachSocket handles reconnect-driven retries via `attemptReconnect`
      // in the close handler; a rejection here is benign.
    });
    return true;
  }

  private attemptReconnect(): void {
    if (this.disposed) return;
    const session = this.currentSession();
    if (!session) {
      this.emit({ type: "reconnectFailed" });
      return;
    }
    if (this.reconnectAttempt >= this.maxReconnectAttempts) {
      this.emit({ type: "reconnectFailed" });
      return;
    }
    this.reconnectAttempt++;
    const delay = Math.min(Math.pow(2, this.reconnectAttempt - 1) * 1000, 5000);
    this.emit({
      type: "reconnecting",
      attempt: this.reconnectAttempt,
      maxAttempts: this.maxReconnectAttempts,
    });
    this.reconnectTimer = setTimeout(() => {
      this.tryReconnect(session);
    }, delay);
  }

  private startPing(): void {
    if (this.pingInterval) {
      clearInterval(this.pingInterval);
    }
    this.pingInterval = setInterval(() => {
      this.send({ type: "Ping", data: { timestamp: Date.now() } });
    }, 5000);
  }

  private nativeAiSetupFrame(options: NativeAiAdapterOptions) {
    return {
      type: "CreateGameWithSettings",
      data: {
        deck: this.deckData,
        display_name: this.displayName,
        public: false,
        password: null,
        timer_seconds: null,
        player_count: options.playerCount,
        match_config: options.matchConfig ?? { match_type: "Bo1" },
        ai_seats: options.aiSeats.map((seat) => ({
          seatIndex: seat.seatIndex,
          difficulty: seat.difficulty,
          deckName: null,
          deck: { type: "DeckList", data: seat.deck },
        })),
        format_config: options.formatConfig ?? null,
        room_name: null,
        start_when_full: true,
        ranked: false,
      },
    };
  }

  private nativePregameSetupFrame(options: NativePregameAdapterOptions): unknown {
    if (options.kind === "host") {
      return {
        type: "CreateGameWithSettings",
        data: {
          deck: this.deckData,
          display_name: this.displayName,
          public: false,
          password: null,
          timer_seconds: null,
          player_count: options.playerCount,
          match_config: options.matchConfig ?? { match_type: "Bo1" },
          ai_seats: options.aiSeats.map((seat) => ({
            seatIndex: seat.seatIndex,
            difficulty: seat.difficulty,
            deck: { type: "DeckList", data: seat.deck },
          })),
          format_config: options.formatConfig ?? null,
          start_when_full: false,
          ranked: false,
        },
      };
    }
    if (options.kind === "guest") {
      return {
      type: "JoinGameWithPassword",
      data: {
        game_code: this.joinGameCode!,
        deck: this.deckData,
        display_name: this.displayName,
        password: this.joinPassword ?? null,
        reservation_token: this.reservationToken ?? null,
      },
      };
    }
    return {
      type: "Reconnect",
      data: {
        game_code: options.gameCode,
        player_token: options.playerToken,
        full_key: options.fullKey,
      },
    };
  }

  /** Seeds persisted native credentials before either initialization path
   * attaches the socket, so its first reconnect response can be authenticated. */
  private seedNativeReconnectSession(): void {
    const options = this.options.nativePregame;
    if (options?.kind !== "reconnect") return;

    this._gameCode = options.gameCode;
    this._playerId = options.playerId;
    this.playerToken = options.playerToken;
    this.fullSessionKey = options.fullKey;
  }

  private nativeSocketOptions(): NativeSocketAdapterOptions | null {
    return this.options.nativeAi ?? this.options.nativePregame ?? null;
  }

  private isNativeSocket(): boolean {
    return this.nativeSocketOptions() !== null;
  }

  private rejectInitialization(error: Error): void {
    if (this.initReject) {
      this.initReject(error);
      this.initResolve = null;
      this.initReject = null;
    }
    if (this.pregameReject) {
      this.pregameReject(error);
      this.pregameResolve = null;
      this.pregameReject = null;
    }
    if (this.gameStartedReject) {
      this.gameStartedReject(error);
      this.gameStartedResolve = null;
      this.gameStartedReject = null;
    }
  }

  private rejectPregameMutation(error: Error): void {
    this.pregameMutationReject?.(error);
    this.pregameMutationResolve = null;
    this.pregameMutationReject = null;
    this.pregameMutationSlotsRevision = null;
    this.playerSlotsReject?.(error);
    this.playerSlotsResolve = null;
    this.playerSlotsReject = null;
    this.playerSlotsTargetRevision = null;
  }

  private rejectAbandon(error: Error): void {
    this.abandonReject?.(error);
    this.abandonResolve = null;
    this.abandonReject = null;
  }

  /**
   * Serialize and send a frame. Returns `false` (and emits an `error` event)
   * instead of throwing when the socket is missing/closed or `WebSocket.send`
   * throws, so callers — especially `submitAction` — can recover rather than
   * leaving the adapter wedged. Mirrors the guarded send in `PeerSession`.
   */
  private send(msg: unknown): boolean {
    const ws = this.ws;
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

  private rejectPendingManaPaymentPreviews(error: Error): void {
    for (const { reject } of this.pendingManaPaymentPreviews.values()) {
      reject(error);
    }
    this.pendingManaPaymentPreviews.clear();
  }

  /** Snapshot of the server's advertised identity, or null before ServerHello. */
  getServerInfo(): ServerInfo | null {
    return this._serverInfo;
  }

  private handleMessage(msg: { type: string; data?: unknown }): void {
    if (this.sessionIdentityRejected) return;

    switch (msg.type) {
      // ServerHello is no longer observed here — the shared
      // `openPhaseSocket` helper consumes it during `attachSocket`, and
      // `_serverInfo` / the `serverHello` event are populated before the
      // post-handshake message loop begins.

      case "GameCreated": {
        const data = msg.data as {
          game_code: string;
          player_token: string;
          full_key?: FullSessionKey;
        };
        if (!this.acceptNativeReconnectIdentity({
          gameCode: data.game_code,
          playerToken: data.player_token,
          fullKey: data.full_key,
        })) break;
        this._gameCode = data.game_code;
        this.playerToken = data.player_token;
        if (data.full_key && !this.acceptFullSessionKey(data.full_key)) break;
        this.hostWaitingForOpponent = true;
        this.emit({ type: "sessionChanged", session: this.currentSession() });
        this.emit({ type: "gameCreated", gameCode: data.game_code });
        this.emit({ type: "waitingForOpponent" });
        break;
      }

      case "SessionAttached": {
        const data = msg.data as {
          game_code: string;
          player_id: PlayerId;
          player_token: string;
          full_key?: FullSessionKey;
        };
        if (!this.acceptNativeReconnectIdentity({
          gameCode: data.game_code,
          playerId: data.player_id,
          playerToken: data.player_token,
          fullKey: data.full_key,
        })) break;
        this._gameCode = data.game_code;
        const fullKey = data.full_key;
        if (!fullKey) {
          this.acceptFullSessionKey(undefined);
          break;
        }
        if (!this.acceptFullSessionKey(fullKey)) break;
        const attachment: NativeSessionAttachment = {
          gameCode: data.game_code,
          playerId: data.player_id,
          playerToken: data.player_token,
          fullKey,
        };
        this._playerId = attachment.playerId;
        this.playerToken = attachment.playerToken;
        this.emit({ type: "sessionChanged", session: this.currentSession() });
        this.emit({ type: "sessionAttached", attachment });
        if (this.pregameResolve) {
          this.pregameResolve(attachment);
          this.pregameResolve = null;
          this.pregameReject = null;
        }
        break;
      }

      case "GameAbandoned": {
        this.abandonResolve?.();
        this.abandonResolve = null;
        this.abandonReject = null;
        break;
      }

      case "PlayerSlotsUpdate": {
        this.playerSlotsRevision++;
        if (
          this.pregameMutationResolve
          && this.pregameMutationSlotsRevision !== null
          && this.playerSlotsRevision > this.pregameMutationSlotsRevision
        ) {
          this.pregameMutationResolve();
          this.pregameMutationResolve = null;
          this.pregameMutationReject = null;
          this.pregameMutationSlotsRevision = null;
        }
        if (
          this.playerSlotsResolve
          && this.playerSlotsTargetRevision !== null
          && this.playerSlotsRevision >= this.playerSlotsTargetRevision
        ) {
          this.playerSlotsResolve();
          this.playerSlotsResolve = null;
          this.playerSlotsReject = null;
          this.playerSlotsTargetRevision = null;
        }
        break;
      }

      case "PasswordRequired": {
        // Server says: this room is password-protected and the client
        // either sent no password or a wrong one. Surface an event so the
        // UI can prompt, and reject init so callers know the join failed
        // for a recoverable reason. Recoverable because the UI just needs
        // to collect a password and create a fresh adapter with it.
        //
        // Reconnect path: if this arrives while `reconnectInFlight` (e.g.
        // server restarted and re-demands the password), clear the flag
        // and surface `reconnectFailed` so the UI stops retrying silently.
        // Otherwise the adapter would stay stuck waiting for a
        // `GameStarted` that will never come.
        const data = msg.data as { game_code: string };
        this.emit({ type: "passwordRequired", gameCode: data.game_code });
        if (this.reconnectInFlight) {
          this.reconnectInFlight = false;
          this.reconnectAttempt = 0;
          this.emit({ type: "reconnectFailed" });
        }
        if (this.initReject) {
          this.initReject(
            new AdapterError(
              "PASSWORD_REQUIRED",
              "Room requires a password",
              true,
            ),
          );
          this.initResolve = null;
          this.initReject = null;
        }
        break;
      }

      case "GameStarted": {
        const data = msg.data as { state_revision: number; state: GameState; your_player: PlayerId; opponent_name?: string; player_names?: string[]; legal_actions?: GameAction[]; auto_pass_recommended?: boolean; end_continuous_effect_offers?: LegalActionsResult["endContinuousEffectOffers"]; mana_payment_shortcut_actions?: GameAction[]; spell_costs?: Record<string, ManaCost>; legal_actions_by_object?: Record<string, GameAction[]>; viewer_interaction?: LegalActionsResult["viewerInteraction"]; derived?: GameState["derived"]; player_token?: string; full_key?: FullSessionKey; events?: GameEvent[]; rewind_targets?: RewindOption[] };
        const nativeReconnect = this.options.nativePregame?.kind === "reconnect"
          ? this.options.nativePregame
          : null;
        if (!this.acceptNativeReconnectIdentity({
          playerId: data.your_player,
          playerToken: data.player_token,
          fullKey: data.full_key,
        })) break;
        if (nativeReconnect) {
          if (!this.acceptFullSessionKey(data.full_key)) break;
        }
        if (this.reconnectInFlight) {
          this.reconnectInFlight = false;
          this.reconnectAttempt = 0;
          this.emit({ type: "reconnected" });
        } else if (this.hostWaitingForOpponent) {
          this.hostWaitingForOpponent = false;
          this.emit({
            type: "opponentJoined",
            opponentName: data.opponent_name,
          });
        }
        const startedSnapshot = this.cacheSnapshot(
          { ...data.state, derived: data.derived ?? data.state.derived },
          {
            actions: data.legal_actions ?? [],
            autoPassRecommended: data.auto_pass_recommended ?? false,
            endContinuousEffectOffers: data.end_continuous_effect_offers ?? [],
            manaPaymentShortcutActions: data.mana_payment_shortcut_actions ?? [],
            spellCosts: data.spell_costs,
            legalActionsByObject: data.legal_actions_by_object,
            viewerInteraction: data.viewer_interaction,
          },
        );
        this._playerId = data.your_player;
        if (nativeReconnect) {
          const attachment: NativeSessionAttachment = {
            gameCode: nativeReconnect.gameCode,
            playerId: nativeReconnect.playerId,
            playerToken: nativeReconnect.playerToken,
            fullKey: nativeReconnect.fullKey,
          };
          this.emit({ type: "sessionChanged", session: this.currentSession() });
          this.emit({ type: "sessionAttached", attachment });
          this.pregameResolve?.(attachment);
          this.pregameResolve = null;
          this.pregameReject = null;
        }
        this.receivedGameStarted = true;
        // Joiners receive their player_token here (hosts get it via GameCreated).
        // Set _gameCode from joinGameCode if not already set (host sets it via GameCreated).
        if (!this._gameCode && this.joinGameCode) {
          this._gameCode = this.joinGameCode;
        }
        if (!nativeReconnect && data.full_key && !this.acceptFullSessionKey(data.full_key)) break;
        if (!nativeReconnect && data.player_token) {
          this.playerToken = data.player_token;
          this.emit({ type: "sessionChanged", session: this.currentSession() });
        }
        const playerNames = data.player_names === undefined
          ? undefined
          : playerNamesFromWire(data.player_names);
        this.emit({
          type: "playerIdentity",
          playerId: data.your_player,
          opponentName: data.opponent_name ?? null,
          ...(playerNames === undefined ? {} : { playerNames }),
        });
        const initializedNow = this.initResolve !== null;
        if (this.initResolve) {
          // CR 103.1: the server sends the StartingPlayerContest event only on
          // the initial GameStarted (drained server-side, so reconnects carry
          // none). Stash it for initializeGame() to return, routing it through
          // the same gameStore.initGame contest path as local games.
          this.initStartEvents = data.events ?? [];
          this.initResolve();
          this.initResolve = null;
          this.initReject = null;
        }
        if (this.gameStartedResolve) {
          this.gameStartedResolve();
          this.gameStartedResolve = null;
          this.gameStartedReject = null;
        }
        // Always an array, never `undefined`: on this transport `undefined`
        // would mean "this transport does not publish", which is false here —
        // an omitted field means the server published none.
        const startedRewindTargets = data.rewind_targets ?? [];
        if (this.options.nativePregame) {
          this.emit({
            type: "stateChanged",
            snapshot: startedSnapshot,
            events: data.events ?? [],
            serverRevision: data.state_revision,
            rewindTargets: startedRewindTargets,
          });
        } else if (!initializedNow) {
          // Reconnect path — no initResolve pending, so emit state change
          // so GameProvider's event listener populates the store. Emits the
          // cached snapshot, which carries the derived-attached state (this
          // emit previously sent the raw `data.state`, dropping `derived`).
          this.emit({
            type: "stateChanged",
            snapshot: startedSnapshot,
            events: [],
            rewindTargets: startedRewindTargets,
          });
        }
        break;
      }

      case "StateUpdate": {
        const data = msg.data as { state_revision: number; state: GameState; events: GameEvent[]; legal_actions?: GameAction[]; auto_pass_recommended?: boolean; end_continuous_effect_offers?: LegalActionsResult["endContinuousEffectOffers"]; mana_payment_shortcut_actions?: GameAction[]; spell_costs?: Record<string, ManaCost>; legal_actions_by_object?: Record<string, GameAction[]>; viewer_interaction?: LegalActionsResult["viewerInteraction"]; log_entries?: GameLogEntry[]; derived?: GameState["derived"]; rewind_targets?: RewindOption[] };
        // Attach the engine-authored derived views to the state snapshot so
        // components (e.g. CommanderDamage) can read them via gameState.derived
        // without a separate subscription path. See
        // crates/engine/src/game/derived_views.rs.
        const updateSnapshot = this.cacheSnapshot(
          { ...data.state, derived: data.derived ?? data.state.derived },
          {
            actions: data.legal_actions ?? [],
            autoPassRecommended: data.auto_pass_recommended ?? false,
            endContinuousEffectOffers: data.end_continuous_effect_offers ?? [],
            manaPaymentShortcutActions: data.mana_payment_shortcut_actions ?? [],
            spellCosts: data.spell_costs,
            legalActionsByObject: data.legal_actions_by_object,
            viewerInteraction: data.viewer_interaction,
          },
        );
        const resolvedAction = this.pendingResolve !== null;
        if (this.pendingResolve) {
          this.emit({ type: "actionPendingChanged", pending: false });
          this.pendingResolve({ events: data.events, log_entries: data.log_entries });
          this.pendingResolve = null;
          this.pendingReject = null;
        }
        if (!resolvedAction || this.options.nativePregame) {
          this.emit({
            type: "stateChanged",
            snapshot: updateSnapshot,
            events: data.events,
            logEntries: data.log_entries,
            serverRevision: data.state_revision,
            rewindTargets: data.rewind_targets ?? [],
          });
        }
        break;
      }

      case "ActionRejected": {
        const data = msg.data as { reason: string };
        this.emit({ type: "actionPendingChanged", pending: false });
        if (this.pendingReject) {
          this.pendingReject(
            actionRejectionError(data.reason),
          );
          this.pendingResolve = null;
          this.pendingReject = null;
        } else {
          // No in-flight action owns this rejection, so it answers a
          // fire-and-forget request — `sendRequestTakeback` is the only one
          // today. Surface it instead of dropping it on the floor.
          //
          // These two branches cannot race: `handle_client_message` is
          // awaited to completion before the socket reads the next frame, so
          // a takeback refusal is not even parsed until any preceding action
          // has been fully answered. That is what rules out the sharper
          // hazard here — rejecting an in-flight ACTION's promise with a
          // TAKEBACK's reason string, which would be a misattribution rather
          // than merely a stale spinner.
          this.emit({ type: "requestRejected", reason: data.reason });
        }
        break;
      }

      case "ResolveAllResult": {
        const data = msg.data as {
          request_id: number;
          items_resolved: number;
          total: number;
        };
        if (this.pendingResolveAll?.requestId === data.request_id) {
          const waitingFor = this.snapshot?.state.waiting_for;
          if (!waitingFor) {
            this.pendingResolveAll.reject(
              new AdapterError("WS_ERROR", "Resolve All result arrived without a state snapshot", false),
            );
            this.pendingResolveAll = null;
            break;
          }
          this.pendingResolveAll.resolve({
            events: [],
            waitingFor,
            logEntries: [],
            itemsResolved: data.items_resolved,
            total: data.total,
          });
          this.pendingResolveAll = null;
        }
        break;
      }

      case "ResolveAllRejected": {
        const data = msg.data as { request_id: number; reason: string };
        if (this.pendingResolveAll?.requestId === data.request_id) {
          this.pendingResolveAll.reject(resolveAllRejectionError(data.reason));
          this.pendingResolveAll = null;
        }
        break;
      }

      case "ActionNoOp": {
        this.emit({ type: "actionPendingChanged", pending: false });
        if (this.pendingResolve) {
          this.pendingResolve({ events: [], log_entries: [] });
          this.pendingResolve = null;
          this.pendingReject = null;
        }
        break;
      }

      case "ManaPaymentPreview": {
        const data = msg.data as { request_id: number; source_ids: ObjectId[] };
        const pending = this.pendingManaPaymentPreviews.get(data.request_id);
        if (pending) {
          this.pendingManaPaymentPreviews.delete(data.request_id);
          pending.resolve(data.source_ids);
        }
        break;
      }

      case "ManaPaymentPreviewRejected": {
        const data = msg.data as { request_id: number; reason: string };
        const pending = this.pendingManaPaymentPreviews.get(data.request_id);
        if (pending) {
          this.pendingManaPaymentPreviews.delete(data.request_id);
          pending.reject(actionRejectionError(data.reason));
        }
        break;
      }

      case "OpponentDisconnected": {
        const data = msg.data as { grace_seconds: number };
        this.emit({
          type: "opponentDisconnected",
          graceSeconds: data.grace_seconds,
        });
        break;
      }

      case "OpponentReconnected": {
        this.emit({ type: "opponentReconnected" });
        break;
      }

      case "TerminalResult": {
        const delivery = (msg.data as { delivery?: FullTerminalDelivery }).delivery;
        if (!delivery) break;
        void (async () => {
          if (!(await commitFullTerminalDelivery(delivery))) {
            this.emit({
              type: "terminalUnavailable",
              message: "Failed to retain terminal delivery",
            });
            return;
          }
          this.gameEnded = true;
          this.emit({ type: "actionPendingChanged", pending: false });
          this.emit({ type: "sessionChanged", session: null });
          this.emit({ type: "terminalDelivery", delivery });
          await acknowledgeFullTerminalDelivery(
            this.serverUrl,
            delivery.delivery_id,
            delivery.credential,
          );
        })().catch((error: unknown) => {
          this.emit({
            type: "terminalUnavailable",
            message: error instanceof Error ? error.message : "Terminal acknowledgement failed",
          });
        });
        break;
      }

      case "GameOver": {
        const data = msg.data as { winner: PlayerId | null; reason: string };
        this.gameEnded = true;
        this.emit({ type: "actionPendingChanged", pending: false });
        this.emit({ type: "sessionChanged", session: null });
        this.emit({
          type: "gameOver",
          winner: data.winner,
          reason: data.reason,
        });
        break;
      }

      case "Conceded": {
        const data = msg.data as { player: PlayerId };
        this.emit({ type: "conceded", player: data.player });
        break;
      }

      case "Emote": {
        const data = msg.data as { from_player: PlayerId; emote: string };
        this.emit({
          type: "emoteReceived",
          fromPlayer: data.from_player,
          emote: data.emote,
        });
        break;
      }

      case "TimerUpdate": {
        const data = msg.data as { player: PlayerId; remaining_seconds: number };
        this.emit({
          type: "timerUpdate",
          player: data.player,
          remainingSeconds: data.remaining_seconds,
        });
        break;
      }

      case "TakebackRequested": {
        const data = msg.data as { requester: PlayerId; requester_name: string };
        this.emit({
          type: "takebackRequested",
          requester: data.requester,
          requesterName: data.requester_name,
        });
        break;
      }

      case "TakebackResolved": {
        const data = msg.data as { approved: boolean; resolved_by?: PlayerId | null };
        this.emit({
          type: "takebackResolved",
          approved: data.approved,
          resolvedBy: data.resolved_by ?? null,
        });
        break;
      }

      case "PlayerDisconnected": {
        const data = msg.data as { player_id: PlayerId; grace_seconds: number };
        this.emit({
          type: "playerDisconnected",
          playerId: data.player_id,
          graceSeconds: data.grace_seconds,
        });
        break;
      }

      case "PlayerReconnected": {
        const data = msg.data as { player_id: PlayerId };
        this.emit({ type: "playerReconnected", playerId: data.player_id });
        break;
      }

      case "GamePaused": {
        const data = msg.data as { disconnected_player: PlayerId; timeout_seconds: number };
        this.emit({
          type: "gamePaused",
          disconnectedPlayer: data.disconnected_player,
          timeoutSeconds: data.timeout_seconds,
        });
        break;
      }

      case "GameResumed": {
        this.emit({ type: "gameResumed" });
        break;
      }

      case "PlayerEliminated": {
        const data = msg.data as { player_id: PlayerId };
        this.emit({
          type: "playerEliminated",
          playerId: data.player_id,
          becameSpectator: data.player_id === this._playerId,
        });
        break;
      }

      case "SpectatorJoined": {
        const data = msg.data as { name: string };
        this.emit({ type: "spectatorJoined", name: data.name });
        break;
      }

      case "Pong": {
        const data = msg.data as { timestamp: number };
        const rtt = Date.now() - data.timestamp;
        this.emit({ type: "latencyChanged", latencyMs: rtt });
        break;
      }

      case "Error": {
        const data = msg.data as { message: string; code?: string };
        const initializationError = data.code === "deck_rejected"
          ? new AdapterError(AdapterErrorCode.DECK_REJECTED, data.message, false)
          : actionRejectionError(data.message);
        this.rejectInitialization(initializationError);
        this.rejectPregameMutation(actionRejectionError(data.message));
        this.rejectAbandon(actionRejectionError(data.message));
        this.emit({ type: "error", message: data.message });
        break;
      }
    }
  }

  private currentSession(): WsSessionData | null {
    if (!this._gameCode || !this.playerToken || !this.fullSessionKey) {
      return null;
    }
    return {
      gameCode: this._gameCode,
      playerToken: this.playerToken,
      fullKey: this.fullSessionKey,
      serverUrl: this.serverUrl,
      timestamp: Date.now(),
    };
  }

  /** Rejects missing or changed Full identities before they can be persisted. */
  private acceptFullSessionKey(key: FullSessionKey | undefined): boolean {
    if (!key || key.game_code !== this._gameCode || key.generation < 1) {
      const error = new AdapterError("WS_ERROR", "Server omitted a valid Full session identity", false);
      return this.rejectSessionIdentity(error);
    }
    if (
      this.fullSessionKey
      && (this.fullSessionKey.game_code !== key.game_code
        || this.fullSessionKey.generation !== key.generation)
    ) {
      const error = new AdapterError("WS_ERROR", "Server changed the Full session identity", false);
      return this.rejectSessionIdentity(error);
    }
    this.fullSessionKey = key;
    return true;
  }

  /** Reject identity-bearing reconnect frames before they can update session state. */
  private acceptNativeReconnectIdentity(frame: {
    gameCode?: string;
    playerId?: PlayerId;
    playerToken?: string;
    fullKey?: FullSessionKey;
  }): boolean {
    const expected = this.options.nativePregame?.kind === "reconnect"
      ? this.options.nativePregame
      : null;
    if (!expected) return true;

    const errorMessage = frame.gameCode !== undefined && frame.gameCode !== expected.gameCode
      ? `Native reconnect attached game ${frame.gameCode}, expected ${expected.gameCode}`
      : frame.playerId !== undefined && frame.playerId !== expected.playerId
        ? `Native reconnect attached player ${frame.playerId}, expected ${expected.playerId}`
        : frame.playerToken !== undefined && frame.playerToken !== expected.playerToken
          ? "Native reconnect changed the player token"
          : !frame.fullKey
            ? "Server omitted a valid Full session identity"
            : frame.fullKey.game_code !== expected.fullKey.game_code
                || frame.fullKey.generation !== expected.fullKey.generation
              ? "Server changed the Full session identity"
              : null;
    if (!errorMessage) return true;

    return this.rejectSessionIdentity(new AdapterError("WS_ERROR", errorMessage, false));
  }

  /** Latches an invalid Full identity before later frames can mutate session state. */
  private rejectSessionIdentity(error: AdapterError): false {
    if (this.sessionIdentityRejected) return false;

    this.sessionIdentityRejected = true;
    if (this.pingInterval) {
      clearInterval(this.pingInterval);
      this.pingInterval = null;
    }
    this.rejectInitialization(error);
    this.emit({ type: "error", message: error.message });
    this.ws?.close();
    return false;
  }
}
