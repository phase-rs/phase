import type {
  EngineAdapter,
  GameAction,
  GameEvent,
  GameLogEntry,
  GameState,
  PlayerId,
  SubmitResult,
} from "./types";
import { AdapterError, AdapterErrorCode } from "./types";
import { useMultiplayerStore } from "../stores/multiplayerStore";

/** Deck data format matching server protocol. */
export interface DeckData {
  main_deck: string[];
  sideboard: string[];
}

/** Events emitted by the WebSocketAdapter for UI state updates. */
export type WsAdapterEvent =
  | { type: "gameCreated"; gameCode: string }
  | { type: "waitingForOpponent" }
  | { type: "opponentDisconnected"; graceSeconds: number }
  | { type: "opponentReconnected" }
  | { type: "playerDisconnected"; playerId: PlayerId; graceSeconds: number }
  | { type: "playerReconnected"; playerId: PlayerId }
  | { type: "gamePaused"; disconnectedPlayer: PlayerId; timeoutSeconds: number }
  | { type: "gameResumed" }
  | { type: "playerEliminated"; playerId: PlayerId }
  | { type: "spectatorJoined"; name: string }
  | { type: "gameOver"; winner: PlayerId | null; reason: string }
  | { type: "error"; message: string }
  | { type: "reconnecting"; attempt: number; maxAttempts: number }
  | { type: "reconnected" }
  | { type: "reconnectFailed" }
  | { type: "stateChanged"; state: GameState; events: GameEvent[]; legalActions: GameAction[] }
  | { type: "emoteReceived"; fromPlayer: PlayerId; emote: string }
  | { type: "conceded"; player: PlayerId }
  | { type: "timerUpdate"; player: PlayerId; remainingSeconds: number };

type WsAdapterEventListener = (event: WsAdapterEvent) => void;

const WS_STORAGE_KEY = "phase-ws-session";

interface SessionData {
  gameCode: string;
  playerToken: string;
}

/**
 * WebSocket-backed implementation of EngineAdapter.
 * Communicates with the phase-server via WebSocket protocol
 * for multiplayer games.
 */
export class WebSocketAdapter implements EngineAdapter {
  private ws: WebSocket | null = null;
  private gameState: GameState | null = null;
  private _playerId: PlayerId | null = null;
  private _legalActions: GameAction[] = [];
  private playerToken: string | null = null;
  private _gameCode: string | null = null;
  private pendingResolve: ((result: SubmitResult) => void) | null = null;
  private pendingReject: ((error: Error) => void) | null = null;
  private initResolve: (() => void) | null = null;
  private initReject: ((error: Error) => void) | null = null;
  private listeners: WsAdapterEventListener[] = [];
  private reconnectAttempt = 0;
  private readonly maxReconnectAttempts = 3;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private disposed = false;

  constructor(
    private readonly serverUrl: string,
    private readonly mode: "host" | "join",
    private readonly deckData: DeckData,
    private readonly joinGameCode?: string,
    private readonly joinPassword?: string,
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
  ): Promise<SubmitResult> {
    // Server handles deck data via WebSocket protocol during initialize()
    return { events: [] };
  }

  async initialize(): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      this.initResolve = resolve;
      this.initReject = reject;

      this.ws = new WebSocket(this.serverUrl);

      this.ws.onopen = () => {
        if (this.mode === "host") {
          this.send({
            type: "CreateGame",
            data: { deck: this.deckData },
          });
        } else {
          const displayName = useMultiplayerStore.getState().displayName;
          this.send({
            type: "JoinGameWithPassword",
            data: {
              game_code: this.joinGameCode!,
              deck: this.deckData,
              display_name: displayName || "Player",
              password: this.joinPassword ?? null,
            },
          });
        }
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

  async submitAction(action: GameAction): Promise<SubmitResult> {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new AdapterError("WS_ERROR", "WebSocket not connected", false);
    }

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

  async getLegalActions(): Promise<GameAction[]> {
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
    this.listeners = [];
    sessionStorage.removeItem(WS_STORAGE_KEY);
  }

  /** Attempt reconnection using stored session data. */
  tryReconnect(): boolean {
    const raw = sessionStorage.getItem(WS_STORAGE_KEY);
    if (!raw) return false;

    const session: SessionData = JSON.parse(raw);
    this._gameCode = session.gameCode;
    this.playerToken = session.playerToken;

    this.ws = new WebSocket(this.serverUrl);
    this.ws.onopen = () => {
      this.send({
        type: "Reconnect",
        data: {
          game_code: session.gameCode,
          player_token: session.playerToken,
        },
      });
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
      if (this.gameState !== null) {
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
    if (this.reconnectAttempt >= this.maxReconnectAttempts) {
      this.emit({ type: "reconnectFailed" });
      return;
    }
    this.reconnectAttempt++;
    const delay = Math.pow(2, this.reconnectAttempt - 1) * 1000;
    this.emit({
      type: "reconnecting",
      attempt: this.reconnectAttempt,
      maxAttempts: this.maxReconnectAttempts,
    });
    this.reconnectTimer = setTimeout(() => {
      this.tryReconnect();
    }, delay);
  }

  private send(msg: unknown): void {
    this.ws?.send(JSON.stringify(msg));
  }

  private handleMessage(msg: { type: string; data?: unknown }): void {
    switch (msg.type) {
      case "GameCreated": {
        const data = msg.data as { game_code: string; player_token: string };
        this._gameCode = data.game_code;
        this.playerToken = data.player_token;
        this.persistSession();
        this.emit({ type: "gameCreated", gameCode: data.game_code });
        this.emit({ type: "waitingForOpponent" });
        break;
      }

      case "GameStarted": {
        const data = msg.data as { state: GameState; your_player: PlayerId; opponent_name?: string; legal_actions?: GameAction[] };
        this.gameState = data.state;
        this._playerId = data.your_player;
        this._legalActions = data.legal_actions ?? [];
        useMultiplayerStore.getState().setActivePlayerId(data.your_player);
        useMultiplayerStore.getState().setOpponentDisplayName(data.opponent_name ?? null);
        if (this.initResolve) {
          this.initResolve();
          this.initResolve = null;
          this.initReject = null;
        } else {
          // Reconnect path — no initResolve pending, so emit state change
          // so GameProvider's event listener populates the store.
          this.emit({ type: "stateChanged", state: data.state, events: [], legalActions: this._legalActions });
        }
        break;
      }

      case "StateUpdate": {
        const data = msg.data as { state: GameState; events: GameEvent[]; legal_actions?: GameAction[]; log_entries?: GameLogEntry[] };
        this.gameState = data.state;
        this._legalActions = data.legal_actions ?? [];
        if (this.pendingResolve) {
          this.pendingResolve({ events: data.events, log_entries: data.log_entries });
          this.pendingResolve = null;
          this.pendingReject = null;
        } else {
          this.emit({ type: "stateChanged", state: data.state, events: data.events, legalActions: this._legalActions });
        }
        break;
      }

      case "ActionRejected": {
        const data = msg.data as { reason: string };
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
        this.emit({ type: "playerEliminated", playerId: data.player_id });
        // Auto-transition to spectator if the eliminated player is us
        if (data.player_id === this._playerId) {
          useMultiplayerStore.getState().setIsSpectator(true);
          useMultiplayerStore.getState().showToast("You have been eliminated. Now spectating.");
        }
        break;
      }

      case "SpectatorJoined": {
        const data = msg.data as { name: string };
        this.emit({ type: "spectatorJoined", name: data.name });
        break;
      }

      case "Error": {
        const data = msg.data as { message: string };
        this.emit({ type: "error", message: data.message });
        break;
      }
    }
  }

  private persistSession(): void {
    if (this._gameCode && this.playerToken) {
      const session: SessionData = {
        gameCode: this._gameCode,
        playerToken: this.playerToken,
      };
      sessionStorage.setItem(WS_STORAGE_KEY, JSON.stringify(session));
    }
  }
}
