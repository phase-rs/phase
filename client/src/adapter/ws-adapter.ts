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
export const PROTOCOL_VERSION = 1;

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
   * Work deferred until a compatible `ServerHello` is received — typically
   * the create-game, join-game, or reconnect frame. Both `initialize()`
   * and `tryReconnect()` populate this so their setup frame never goes out
   * before the handshake completes.
   */
  private pendingHelloContinuation: (() => void) | null = null;

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

      this.ws = new WebSocket(this.serverUrl);

      this.ws.onopen = () => {
        this.startPing();
        this.pendingHelloContinuation = () => {
          if (this.mode === "host") {
            this.send({
              type: "CreateGame",
              data: { deck: this.deckData },
            });
          } else {
            this.send({
              type: "JoinGameWithPassword",
              data: {
                game_code: this.joinGameCode!,
                deck: this.deckData,
                display_name: this.displayName,
                password: this.joinPassword ?? null,
              },
            });
          }
        };
        this.sendClientHello();
      };

      this.ws.onmessage = (event) => {
        this.handleMessage(JSON.parse(event.data as string));
      };

      this.ws.onerror = () => {
        const err = new AdapterError(
          "WS_ERROR",
          "WebSocket connection failed",
          true,
        );
        if (this.initReject) {
          this.initReject(err);
          this.initResolve = null;
          this.initReject = null;
        }
      };

      this.ws.onclose = () => {
        if (this.pingInterval) {
          clearInterval(this.pingInterval);
          this.pingInterval = null;
        }
        // Clear any pending action state — the server may have already processed
        // the action but the response was lost with the connection.
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
        } else if (this.gameState !== null) {
          this.attemptReconnect();
        }
      };
    });
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

  getAiAction(_difficulty: string): GameAction | null {
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
    this.pendingHelloContinuation = null;
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

    this.ws = new WebSocket(this.serverUrl);
    this.ws.onopen = () => {
      this.startPing();
      this.pendingHelloContinuation = () => {
        this.send({
          type: "Reconnect",
          data: {
            game_code: session.gameCode,
            player_token: session.playerToken,
          },
        });
      };
      this.sendClientHello();
    };
    this.ws.onmessage = (event) => {
      const msg = JSON.parse(event.data as string) as { type: string; data?: unknown };
      if (msg.type === "GameStarted") {
        this.reconnectAttempt = 0;
        this.emit({ type: "reconnected" });
      }
      this.handleMessage(msg);
    };
    this.ws.onclose = () => {
      // Retry if we have an active game OR are mid-reconnect (playerToken set but
      // no gameState yet because the server hasn't responded with GameStarted)
      if (this.gameState !== null || this.playerToken !== null) {
        this.attemptReconnect();
      }
    };
    this.ws.onerror = () => {
      // onclose will fire after onerror, which triggers attemptReconnect
    };
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

  private sendClientHello(): void {
    this.send({
      type: "ClientHello",
      data: {
        client_version: __APP_VERSION__,
        build_commit: __BUILD_HASH__,
        protocol_version: PROTOCOL_VERSION,
      },
    });
  }

  /** Snapshot of the server's advertised identity, or null before ServerHello. */
  getServerInfo(): ServerInfo | null {
    return this._serverInfo;
  }

  private handleMessage(msg: { type: string; data?: unknown }): void {
    switch (msg.type) {
      case "ServerHello": {
        // Servers send ServerHello exactly once per connection. A duplicate
        // frame means either a misbehaving server or a test harness; either
        // way, re-running the continuation would send the setup frame twice.
        if (this._serverInfo) {
          return;
        }
        const data = msg.data as {
          server_version: string;
          build_commit: string;
          protocol_version: number;
          mode: "Full" | "LobbyOnly";
        };
        const info: ServerInfo = {
          version: data.server_version,
          buildCommit: data.build_commit,
          protocolVersion: data.protocol_version,
          mode: data.mode,
        };
        this._serverInfo = info;
        const compatible = info.protocolVersion === PROTOCOL_VERSION;
        this.emit({ type: "serverHello", info, compatible });

        if (!compatible) {
          // Give up before running the deferred setup frame — a mismatched
          // protocol will just produce a rejected frame on the other side.
          this.pendingHelloContinuation = null;
          const err = new AdapterError(
            "WS_ERROR",
            `Server protocol version ${info.protocolVersion} does not match client ${PROTOCOL_VERSION}. Please refresh.`,
            false,
          );
          if (this.initReject) {
            this.initReject(err);
            this.initResolve = null;
            this.initReject = null;
          }
          this.ws?.close();
          return;
        }

        const cont = this.pendingHelloContinuation;
        this.pendingHelloContinuation = null;
        cont?.();
        break;
      }

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
        const data = msg.data as { state: GameState; your_player: PlayerId; opponent_name?: string; legal_actions?: GameAction[]; auto_pass_recommended?: boolean; spell_costs?: Record<string, ManaCost>; player_token?: string };
        this.gameState = data.state;
        this._playerId = data.your_player;
        this._legalActions = {
          actions: data.legal_actions ?? [],
          autoPassRecommended: data.auto_pass_recommended ?? false,
          spellCosts: data.spell_costs,
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
        const data = msg.data as { state: GameState; events: GameEvent[]; legal_actions?: GameAction[]; auto_pass_recommended?: boolean; spell_costs?: Record<string, ManaCost>; log_entries?: GameLogEntry[] };
        this.gameState = data.state;
        this._legalActions = {
          actions: data.legal_actions ?? [],
          autoPassRecommended: data.auto_pass_recommended ?? false,
          spellCosts: data.spell_costs,
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
