import type {
  EngineAdapter,
  EngineSnapshot,
  GameAction,
  GameEvent,
  GameLogEntry,
  GameState,
  LegalActionsResult,
  ManaCost,
  ObjectId,
  PlayerId,
  PersistedGameState,
  SubmitResult,
} from "./types";
import { AdapterError, AdapterErrorCode, EMPTY_LEGAL_ACTIONS, actionRejectionError, nextSnapshotSeq } from "./types";
import type { BracketDeckRequest, BracketEstimate } from "../types/bracketEstimate";
import {
  HandshakeError,
  openPhaseSocket,
  type PhaseSocket,
} from "../services/openPhaseSocket";
import { isValidWebSocketUrl, mixedContentBlockReason } from "../services/serverDetection";
import type { WsSessionData } from "../services/multiplayerSession";

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

/**
 * Wire-protocol version the client speaks. Must match `PROTOCOL_VERSION` in
 * `crates/server-core/src/protocol.rs`. Bump in lockstep when either side
 * adds, removes, renames, or changes the type of a protocol variant field.
 *
 * 20 — Actor-scoped priority-passing settings and filtered per-player state.
 * 19 — Connive exact subject snapshots and resident paused post-replacement
 *      drains changed the serialized full-game state.
 * 17 — Dedicated companion deck slot and typed companion-reveal choices.
 * 16 — Meld pair/attacking-entry choices after the mana-payment preview variants.
 * 15 — Mana-payment preview request/response variants.
 * 14 — PrecastCopyShortcut action and its two WaitingFor variants.
 * 13 — WaitingFor::MulliganBottomCards removed; mulligan bottoming folded
 *      into a MulliganDecisionPhase::BottomCards sub-phase on
 *      WaitingFor::MulliganDecision.
 */
export const PROTOCOL_VERSION = 20;

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
  /** The engine pair travels as one `EngineSnapshot` — see the P2P adapter's
   *  `stateChanged` for why the halves must stay inseparable. */
  | { type: "stateChanged"; snapshot: EngineSnapshot; events: GameEvent[]; logEntries?: GameLogEntry[] }
  | { type: "emoteReceived"; fromPlayer: PlayerId; emote: string }
  | { type: "conceded"; player: PlayerId }
  | { type: "timerUpdate"; player: PlayerId; remainingSeconds: number }
  | { type: "takebackRequested"; requester: PlayerId; requesterName: string }
  | { type: "takebackResolved"; approved: boolean; resolvedBy: PlayerId | null };

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
  private ws: WebSocket | null = null;
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
  private pendingResolve: ((result: SubmitResult) => void) | null = null;
  private pendingReject: ((error: Error) => void) | null = null;
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
  private listeners: WsAdapterEventListener[] = [];
  private reconnectAttempt = 0;
  private readonly maxReconnectAttempts = 8;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private pingInterval: ReturnType<typeof setInterval> | null = null;
  private disposed = false;
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
  ) {}

  get gameCode(): string | null {
    return this._gameCode;
  }

  get playerId(): PlayerId | null {
    return this._playerId;
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

      if (!isValidWebSocketUrl(this.serverUrl)) {
        reject(new AdapterError("WS_ERROR", "Invalid WebSocket URL", false));
        this.initResolve = null;
        this.initReject = null;
        return;
      }

      // A ws:// target from an HTTPS page is blocked by the browser before the
      // handshake — surface why instead of letting it fail as "unreachable".
      const blockReason = mixedContentBlockReason(this.serverUrl);
      if (blockReason) {
        reject(new AdapterError("WS_ERROR", blockReason, false));
        this.initResolve = null;
        this.initReject = null;
        return;
      }

      const setupFrame =
        this.mode === "host"
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

  /**
   * Opens a `PhaseSocket` via the shared handshake helper, caches the
   * `ServerInfo`, wires the post-handshake message/close handlers, and
   * sends `setupFrame`. Used by both `initialize()` and `tryReconnect()`
   * so the handshake policy lives in exactly one place.
   */
  private async attachSocket(setupFrame: unknown): Promise<void> {
    let socket: PhaseSocket;
    try {
      socket = await openPhaseSocket(this.serverUrl);
    } catch (err) {
      if (err instanceof HandshakeError) {
        const retryable = err.kind !== "protocol_mismatch" && err.kind !== "invalid_url";
        const adapterErr = new AdapterError("WS_ERROR", err.message, retryable);
        if (this.initReject) {
          this.initReject(adapterErr);
          this.initResolve = null;
          this.initReject = null;
        }
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
      if (this.initReject) {
        this.initReject(
          new AdapterError("WS_ERROR", String(err), true),
        );
        this.initResolve = null;
        this.initReject = null;
      }
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
      if (this.initReject) {
        this.initReject(err);
        this.initResolve = null;
        this.initReject = null;
      }
    };

    socket.ws.onclose = () => {
      if (this.pingInterval) {
        clearInterval(this.pingInterval);
        this.pingInterval = null;
      }
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
      this.rejectPendingManaPaymentPreviews(
        new AdapterError("WS_CLOSED", "Connection closed during mana-payment preview", true),
      );
      if (this.initReject) {
        this.initReject(
          new AdapterError("WS_CLOSED", "Connection closed before game started", true),
        );
        this.initResolve = null;
        this.initReject = null;
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

  getAiAction(_difficulty: string, _playerId: number): GameAction | null {
    return null;
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

  sendEmote(emote: string): void {
    this.send({ type: "Emote", data: { emote } });
  }

  /** GH #1507: ask every other human player to approve rolling the game
   * back to the state immediately before this player's last action. */
  sendRequestTakeback(): void {
    this.send({ type: "RequestTakeback" });
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

  dispose(): void {
    this.disposed = true;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.pingInterval) {
      clearInterval(this.pingInterval);
      this.pingInterval = null;
    }
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
    this.snapshot = null;
    this._playerId = null;
    this.playerToken = null;
    this._gameCode = null;
    this.pendingResolve = null;
    this.pendingReject = null;
    this.rejectPendingManaPaymentPreviews(
      new AdapterError("WS_CLOSED", "Adapter disposed during mana-payment preview", true),
    );
    this.initResolve = null;
    this.initReject = null;
    this.reconnectInFlight = false;
    this._serverInfo = null;
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

    if (!isValidWebSocketUrl(this.serverUrl)) {
      this.emit({ type: "reconnectFailed" });
      return false;
    }

    this.reconnectInFlight = true;
    this.attachSocket({
      type: "Reconnect",
      data: {
        game_code: session.gameCode,
        player_token: session.playerToken,
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
    switch (msg.type) {
      // ServerHello is no longer observed here — the shared
      // `openPhaseSocket` helper consumes it during `attachSocket`, and
      // `_serverInfo` / the `serverHello` event are populated before the
      // post-handshake message loop begins.

      case "GameCreated": {
        const data = msg.data as { game_code: string; player_token: string };
        this._gameCode = data.game_code;
        this.playerToken = data.player_token;
        this.hostWaitingForOpponent = true;
        this.emit({ type: "sessionChanged", session: this.currentSession() });
        this.emit({ type: "gameCreated", gameCode: data.game_code });
        this.emit({ type: "waitingForOpponent" });
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
        const data = msg.data as { state: GameState; your_player: PlayerId; opponent_name?: string; player_names?: string[]; legal_actions?: GameAction[]; auto_pass_recommended?: boolean; spell_costs?: Record<string, ManaCost>; legal_actions_by_object?: Record<string, GameAction[]>; derived?: GameState["derived"]; player_token?: string; events?: GameEvent[] };
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
            spellCosts: data.spell_costs,
            legalActionsByObject: data.legal_actions_by_object,
          },
        );
        this._playerId = data.your_player;
        // Joiners receive their player_token here (hosts get it via GameCreated).
        // Set _gameCode from joinGameCode if not already set (host sets it via GameCreated).
        if (!this._gameCode && this.joinGameCode) {
          this._gameCode = this.joinGameCode;
        }
        if (data.player_token) {
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
        if (this.initResolve) {
          // CR 103.1: the server sends the StartingPlayerContest event only on
          // the initial GameStarted (drained server-side, so reconnects carry
          // none). Stash it for initializeGame() to return, routing it through
          // the same gameStore.initGame contest path as local games.
          this.initStartEvents = data.events ?? [];
          this.initResolve();
          this.initResolve = null;
          this.initReject = null;
        } else {
          // Reconnect path — no initResolve pending, so emit state change
          // so GameProvider's event listener populates the store. Emits the
          // cached snapshot, which carries the derived-attached state (this
          // emit previously sent the raw `data.state`, dropping `derived`).
          this.emit({ type: "stateChanged", snapshot: startedSnapshot, events: [] });
        }
        break;
      }

      case "StateUpdate": {
        const data = msg.data as { state: GameState; events: GameEvent[]; legal_actions?: GameAction[]; auto_pass_recommended?: boolean; spell_costs?: Record<string, ManaCost>; legal_actions_by_object?: Record<string, GameAction[]>; log_entries?: GameLogEntry[]; derived?: GameState["derived"] };
        // Attach the engine-authored derived views to the state snapshot so
        // components (e.g. CommanderDamage) can read them via gameState.derived
        // without a separate subscription path. See
        // crates/engine/src/game/derived_views.rs.
        const updateSnapshot = this.cacheSnapshot(
          { ...data.state, derived: data.derived ?? data.state.derived },
          {
            actions: data.legal_actions ?? [],
            autoPassRecommended: data.auto_pass_recommended ?? false,
            spellCosts: data.spell_costs,
            legalActionsByObject: data.legal_actions_by_object,
          },
        );
        if (this.pendingResolve) {
          this.emit({ type: "actionPendingChanged", pending: false });
          this.pendingResolve({ events: data.events, log_entries: data.log_entries });
          this.pendingResolve = null;
          this.pendingReject = null;
        } else {
          this.emit({
            type: "stateChanged",
            snapshot: updateSnapshot,
            events: data.events,
            logEntries: data.log_entries,
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
        const data = msg.data as { message: string };
        this.emit({ type: "error", message: data.message });
        if (data.message.includes("Deck not legal") && this.initReject) {
          this.initReject(
            new AdapterError("DECK_REJECTED", data.message, false),
          );
          this.initResolve = null;
          this.initReject = null;
        }
        break;
      }
    }
  }

  private currentSession(): WsSessionData | null {
    if (!this._gameCode || !this.playerToken) {
      return null;
    }
    return {
      gameCode: this._gameCode,
      playerToken: this.playerToken,
      serverUrl: this.serverUrl,
      timestamp: Date.now(),
    };
  }
}
