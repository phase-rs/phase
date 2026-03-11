import type {
  EngineAdapter,
  GameAction,
  GameEvent,
  GameState,
  PlayerId,
} from "./types";
import { AdapterError } from "./types";
import { WasmAdapter } from "./wasm-adapter";
import type { PeerSession } from "../network/peer";
import type { P2PMessage } from "../network/protocol";
import { useMultiplayerStore } from "../stores/multiplayerStore";

/** Events emitted by P2P adapters for UI state updates. */
export type P2PAdapterEvent =
  | { type: "roomCreated"; roomCode: string }
  | { type: "waitingForGuest" }
  | { type: "guestConnected" }
  | { type: "opponentDisconnected"; reason: string }
  | { type: "gameOver"; winner: PlayerId | null; reason: string }
  | { type: "error"; message: string }
  | { type: "stateChanged"; state: GameState; events: GameEvent[] };

type P2PAdapterEventListener = (event: P2PAdapterEvent) => void;

/**
 * Filter game state for the guest player.
 * Hides the host's private zones (hand, library) from the guest's view.
 */
function filterStateForGuest(state: GameState): GameState {
  const clone = JSON.parse(JSON.stringify(state)) as GameState;
  // Player 0 is the host; hide their hand and library from guest
  if (clone.players[0]) {
    clone.players[0].hand = [];
    clone.players[0].library = [];
  }
  return clone;
}

/**
 * Host-side P2P adapter. Runs the WASM engine locally and relays
 * filtered state to the guest via WebRTC DataChannel.
 */
export class P2PHostAdapter implements EngineAdapter {
  private wasm = new WasmAdapter();
  private listeners: P2PAdapterEventListener[] = [];
  private messageUnsub: (() => void) | null = null;
  private disconnectUnsub: (() => void) | null = null;
  private guestDeckResolve: ((deckData: unknown) => void) | null = null;

  constructor(
    private readonly deckData: unknown,
    private readonly session: PeerSession,
  ) {}

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
    await this.wasm.initialize();

    // Listen for guest messages
    this.messageUnsub = this.session.onMessage((msg) => {
      this.handleGuestMessage(msg);
    });

    this.disconnectUnsub = this.session.onDisconnect((reason) => {
      this.emit({ type: "opponentDisconnected", reason });
    });
  }

  async initializeGame(_deckData?: unknown): Promise<GameEvent[]> {
    // Wait for guest's deck data
    const guestDeckData = await new Promise<unknown>((resolve) => {
      this.guestDeckResolve = resolve;
    });

    // Build combined deck payload for WASM
    const deckPayload = {
      ...(this.deckData as Record<string, unknown>),
      opponent_deck: (guestDeckData as Record<string, unknown>).opponent_deck ??
        (guestDeckData as Record<string, unknown>).player_deck,
    };

    const events = this.wasm.initializeGame(deckPayload);
    const state = await this.wasm.getState();

    // Host is player 0
    useMultiplayerStore.getState().setActivePlayerId(0);

    // Send initial state to guest (filtered)
    const filteredState = filterStateForGuest(state);
    this.session.send({
      type: "game_setup",
      state: filteredState,
      events,
    });

    return events;
  }

  async submitAction(action: GameAction): Promise<GameEvent[]> {
    const events = await this.wasm.submitAction(action);
    const state = await this.wasm.getState();

    // Send filtered state update to guest
    const filteredState = filterStateForGuest(state);
    this.session.send({
      type: "state_update",
      state: filteredState,
      events,
    });

    return events;
  }

  async getState(): Promise<GameState> {
    return this.wasm.getState();
  }

  async getLegalActions(): Promise<GameAction[]> {
    return this.wasm.getLegalActions();
  }

  getAiAction(_difficulty: string): GameAction | null {
    return null;
  }

  restoreState(_state: GameState): void {
    throw new AdapterError("P2P_ERROR", "Undo not supported in P2P games", false);
  }

  dispose(): void {
    if (this.messageUnsub) this.messageUnsub();
    if (this.disconnectUnsub) this.disconnectUnsub();
    this.session.close();
    this.wasm.dispose();
    this.listeners = [];
  }

  private async handleGuestMessage(msg: P2PMessage): Promise<void> {
    switch (msg.type) {
      case "guest_deck": {
        if (this.guestDeckResolve) {
          this.guestDeckResolve(msg.deckData);
          this.guestDeckResolve = null;
        }
        break;
      }

      case "action": {
        try {
          const events = await this.wasm.submitAction(msg.action);
          const state = await this.wasm.getState();

          // Send filtered state back to guest
          const filteredState = filterStateForGuest(state);
          this.session.send({
            type: "state_update",
            state: filteredState,
            events,
          });

          // Emit state update locally so host UI updates for opponent actions
          this.emit({ type: "stateChanged", state, events });
        } catch (err) {
          const reason = err instanceof Error ? err.message : String(err);
          this.session.send({ type: "action_rejected", reason });
        }
        break;
      }

      case "concede": {
        this.emit({ type: "gameOver", winner: 0, reason: "Opponent conceded" });
        break;
      }

      default:
        break;
    }
  }
}

/**
 * Guest-side P2P adapter. Receives state from host and sends actions.
 */
export class P2PGuestAdapter implements EngineAdapter {
  private gameState: GameState | null = null;
  private listeners: P2PAdapterEventListener[] = [];
  private pendingResolve: ((events: GameEvent[]) => void) | null = null;
  private pendingReject: ((error: Error) => void) | null = null;
  private initResolve: ((events: GameEvent[]) => void) | null = null;
  private messageUnsub: (() => void) | null = null;
  private disconnectUnsub: (() => void) | null = null;

  constructor(
    private readonly deckData: unknown,
    private readonly session: PeerSession,
  ) {}

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
    // Listen for host messages
    this.messageUnsub = this.session.onMessage((msg) => {
      this.handleHostMessage(msg);
    });

    this.disconnectUnsub = this.session.onDisconnect((reason) => {
      this.emit({ type: "opponentDisconnected", reason });
    });

    // Send deck data to host
    this.session.send({ type: "guest_deck", deckData: this.deckData });
  }

  async initializeGame(_deckData?: unknown): Promise<GameEvent[]> {
    // Guest is player 1
    useMultiplayerStore.getState().setActivePlayerId(1);

    // Wait for game_setup from host
    return new Promise<GameEvent[]>((resolve) => {
      if (this.gameState) {
        // Already received setup (buffered message)
        resolve([]);
      } else {
        this.initResolve = resolve;
      }
    });
  }

  async submitAction(action: GameAction): Promise<GameEvent[]> {
    return new Promise<GameEvent[]>((resolve, reject) => {
      this.pendingResolve = resolve;
      this.pendingReject = reject;
      this.session.send({ type: "action", action });
    });
  }

  async getState(): Promise<GameState> {
    if (!this.gameState) {
      throw new AdapterError("P2P_ERROR", "No game state available", false);
    }
    return this.gameState;
  }

  async getLegalActions(): Promise<GameAction[]> {
    // Host validates actions; guest doesn't need local legal action list
    return [];
  }

  getAiAction(_difficulty: string): GameAction | null {
    return null;
  }

  restoreState(_state: GameState): void {
    throw new AdapterError("P2P_ERROR", "Undo not supported in P2P games", false);
  }

  dispose(): void {
    if (this.messageUnsub) this.messageUnsub();
    if (this.disconnectUnsub) this.disconnectUnsub();
    this.session.close();
    this.gameState = null;
    this.pendingResolve = null;
    this.pendingReject = null;
    this.initResolve = null;
    this.listeners = [];
  }

  private handleHostMessage(msg: P2PMessage): void {
    switch (msg.type) {
      case "game_setup": {
        this.gameState = msg.state;
        if (this.initResolve) {
          this.initResolve(msg.events);
          this.initResolve = null;
        }
        break;
      }

      case "state_update": {
        this.gameState = msg.state;
        if (this.pendingResolve) {
          this.pendingResolve(msg.events);
          this.pendingResolve = null;
          this.pendingReject = null;
        } else {
          // Unsolicited update (opponent's action result)
          this.emit({ type: "stateChanged", state: msg.state, events: msg.events });
        }
        break;
      }

      case "action_rejected": {
        if (this.pendingReject) {
          this.pendingReject(new AdapterError("ACTION_REJECTED", msg.reason, true));
          this.pendingResolve = null;
          this.pendingReject = null;
        }
        break;
      }

      default:
        break;
    }
  }
}
