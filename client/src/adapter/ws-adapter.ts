import type {
  EngineAdapter,
  GameAction,
  GameEvent,
  GameLogEntry,
  GameState,
  LegalActionsResult,
  ManaCost,
  PlayerId,
  SubmitResult,
} from "./types";
import { AdapterError, AdapterErrorCode } from "./types";
import {
  HandshakeError,
  openPhaseSocket,
  type PhaseSocket,
} from "../services/openPhaseSocket";
import { isValidWebSocketUrl } from "../services/serverDetection";
import type { WsSessionData } from "../services/multiplayerSession";

/** Deck data format matching server protocol. */
export interface DeckData {
  main_deck: string[];
  sideboard: string[];
  commander?: string[];
}

/**
 * Wire-protocol version the client speaks. Must match `PROTOCOL_VERSION` in
 * `crates/server-core/src/protocol.rs`. Bump in lockstep when either side
 * adds, removes, renames, or changes the type of a protocol variant field.
 */
export const PROTOCOL_VERSION = 4;

/** Identity advertised by the server in its `ServerHello`. */
export interface ServerInfo {
  version: string;
  buildCommit: string;
  protocolVersion: number;
  mode: "Full" | "LobbyOnly";
}

/** Events emitted by the WebSocketAdapter for UI state updates. */
export type WsAdapterEvent =
  | { type: "serverHello"; info: ServerInfo; compatible: boolean }
  | { type: "playerIdentity"; playerId: PlayerId; opponentName: string | null }
  | { type: "actionPendingChanged"; pending: boolean }
  | { type: "latencyChanged"; latencyMs: number | null }
  | { type: "sessionChanged"; session: WsSessionData | null }
  | { type: "gameCreated"; gameCode: string }
  | { type: "waitingForOpponent" }
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
  | { type: "stateChanged"; state: GameState; events: GameEvent[]; legalResult: LegalActionsResult }
  | { type: "emoteReceived"; fromPlayer: PlayerId; emote: string }
  | { type: "conceded"; player: PlayerId }
  | { type: "timerUpdate"; player: PlayerId; remainingSeconds: number };

type WsAdapterEventListener = (event: WsAdapterEvent) => void;

/**
 * WebSocket-backed implementation of EngineAdapter.
 * Communicates with the phase-server via WebSocket protocol
 * for multiplayer games.
 */
export class WebSocketAdapter implements EngineAdapter {
  private ws: WebSocket | null = null;
  private gameState: GameState | null = null;
  private _playerId: PlayerId | null = null;
  private _legalActions: LegalActionsResult = { actions: [], autoPassRecommended: false };
  private playerToken: string | null = null;
  private _gameCode: string | null = null;
  private pendingResolve: ((result: SubmitResult) => void) | null = null;
  private pendingReject: ((error: Error) => void) | null = null;
  private initResolve: (() => void) | null = null;
  private initReject: ((error: Error) => void) | null = null;
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

  constructor(
    private readonly serverUrl: string,
    private readonly mode: "host" | "join",
    private readonly deckData: DeckData,
    private readonly joinGameCode?: string,
    private readonly joinPassword?: string,
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
    // Server handles deck data via WebSocket protocol during initialize()
    return { events: [] };
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

      const setupFrame =
        this.mode === "host"
          ? { type: "CreateGame", data: { deck: this.deckData } }
          : {
              type: "JoinGameWithPassword",
              data: {
                game_code: this.joinGameCode!,
                deck: this.deckData,
                display_name: this.displayName,
                password: this.joinPassword ?? null,
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
      if (this.pendingReject) {
        this.emit({ type: "actionPendingChanged", pending: false });
        this.pendingReject(
          new AdapterError("WS_CLOSED", "Connection closed during action", true),
        );
        this.pendingResolve = null;
        this.pendingReject = null;
      }
      if (this.initReject) {
        this.initReject(
          new AdapterError("WS_CLOSED", "Connection closed before game started", true),
        );
        this.initResolve = null;
        this.initReject = null;
      } else if (this.gameState !== null || this.playerToken !== null) {
        this.attemptReconnect();
      }
    };

    socket.ws.send(JSON.stringify(setupFrame));
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
      this.send({ type: "Action", data: { action } });
    });
  }

  async getState(): Promise<GameState> {
    if (!this.gameState) {
      throw new AdapterError("WS_ERROR", "No game state available", false);
    }
    return this.gameState;
  }

  getAiAction(_difficulty: string, _playerId: number): GameAction | null {
    return null;
  }

  async getLegalActions(): Promise<LegalActionsResult> {
    return this._legalActions;
  }

  restoreState(_state: GameState): void {
    throw new AdapterError(
      AdapterErrorCode.WASM_ERROR,
      "Undo not supported in multiplayer",
      false,
    );
  }

  sendConcede(): void {
    this.send({ type: "Concede" });
  }

  sendEmote(emote: string): void {
    this.send({ type: "Emote", data: { emote } });
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
    this.gameState = null;
    this._playerId = null;
    this.playerToken = null;
    this._gameCode = null;
    this.pendingResolve = null;
    this.pendingReject = null;
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

  private send(msg: unknown): void {
    this.ws?.send(JSON.stringify(msg));
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
        this.emit({ type: "sessionChanged", session: this.currentSession() });
        this.emit({ type: "gameCreated", gameCode: data.game_code });
        this.emit({ type: "waitingForOpponent" });
        break;
      }

      case "GameStarted": {
        const data = msg.data as { state: GameState; your_player: PlayerId; opponent_name?: string; legal_actions?: GameAction[]; auto_pass_recommended?: boolean; spell_costs?: Record<string, ManaCost>; legal_actions_by_object?: Record<string, GameAction[]>; derived?: GameState["derived"]; player_token?: string };
        if (this.reconnectInFlight) {
          this.reconnectInFlight = false;
          this.reconnectAttempt = 0;
          this.emit({ type: "reconnected" });
        }
        this.gameState = { ...data.state, derived: data.derived ?? data.state.derived };
        this._playerId = data.your_player;
        this._legalActions = {
          actions: data.legal_actions ?? [],
          autoPassRecommended: data.auto_pass_recommended ?? false,
          spellCosts: data.spell_costs,
          legalActionsByObject: data.legal_actions_by_object,
        };
        // Joiners receive their player_token here (hosts get it via GameCreated).
        // Set _gameCode from joinGameCode if not already set (host sets it via GameCreated).
        if (data.player_token) {
          if (!this._gameCode && this.joinGameCode) {
            this._gameCode = this.joinGameCode;
          }
          this.playerToken = data.player_token;
          this.emit({ type: "sessionChanged", session: this.currentSession() });
        }
        this.emit({
          type: "playerIdentity",
          playerId: data.your_player,
          opponentName: data.opponent_name ?? null,
        });
        if (this.initResolve) {
          this.initResolve();
          this.initResolve = null;
          this.initReject = null;
        } else {
          // Reconnect path — no initResolve pending, so emit state change
          // so GameProvider's event listener populates the store.
          this.emit({ type: "stateChanged", state: data.state, events: [], legalResult: this._legalActions });
        }
        break;
      }

      case "StateUpdate": {
        const data = msg.data as { state: GameState; events: GameEvent[]; legal_actions?: GameAction[]; auto_pass_recommended?: boolean; spell_costs?: Record<string, ManaCost>; legal_actions_by_object?: Record<string, GameAction[]>; log_entries?: GameLogEntry[]; derived?: GameState["derived"] };
        // Attach the engine-authored derived views to the state snapshot so
        // components (e.g. CommanderDamage) can read them via gameState.derived
        // without a separate subscription path. See
        // crates/engine/src/game/derived_views.rs.
        this.gameState = { ...data.state, derived: data.derived ?? data.state.derived };
        this._legalActions = {
          actions: data.legal_actions ?? [],
          autoPassRecommended: data.auto_pass_recommended ?? false,
          spellCosts: data.spell_costs,
          legalActionsByObject: data.legal_actions_by_object,
        };
        if (this.pendingResolve) {
          this.emit({ type: "actionPendingChanged", pending: false });
          this.pendingResolve({ events: data.events, log_entries: data.log_entries });
          this.pendingResolve = null;
          this.pendingReject = null;
        } else {
          this.emit({ type: "stateChanged", state: data.state, events: data.events, legalResult: this._legalActions });
        }
        break;
      }

      case "ActionRejected": {
        const data = msg.data as { reason: string };
        this.emit({ type: "actionPendingChanged", pending: false });
        if (this.pendingReject) {
          this.pendingReject(
            new AdapterError("ACTION_REJECTED", data.reason, true),
          );
          this.pendingResolve = null;
          this.pendingReject = null;
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
