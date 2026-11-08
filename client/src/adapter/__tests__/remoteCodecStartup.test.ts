import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ServerDraftAdapter } from "../server-draft-adapter";
import { getSharedAdapter } from "../wasm-adapter";
import { PROTOCOL_VERSION, WebSocketAdapter, type WebSocketAdapterOptions } from "../ws-adapter";
import type { GameState } from "../types";
import type { StateCodecSessionProvider } from "../../services/stateCodecSocket";

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => { resolve = yes; });
  return { promise, resolve };
}

async function drain(): Promise<void> {
  for (let i = 0; i < 60; i++) await Promise.resolve();
}

let startup: Deferred<void>;

/** Only the Worker transport is replaced: the shared facade and RPC client are real. */
class ControlledEngineWorker {
  static instances: ControlledEngineWorker[] = [];
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  readonly sessions = new Set<number>();
  readonly decodedReplies: string[] = [];
  decodeCount = 0;
  private nextSession = 1;
  private terminated = false;

  constructor() {
    ControlledEngineWorker.instances.push(this);
  }

  postMessage(request: { id: number; type: string; sessionId?: number }): void {
    const reply = (data: unknown) => {
      if (!this.terminated) this.onmessage?.(new MessageEvent("message", {
        data: { type: "result", id: request.id, data },
      }));
    };
    switch (request.type) {
      case "init":
        void startup.promise.then(() => reply(null));
        break;
      case "openStateCodec": {
        const session = this.nextSession++;
        this.sessions.add(session);
        reply(session);
        break;
      }
      case "decodeStateCodec": {
        if (!this.sessions.has(request.sessionId!)) throw new Error("Decoder lease is not live");
        const json = this.decodedReplies.shift();
        if (json === undefined) throw new Error("No worker decode response was supplied");
        this.decodeCount++;
        reply({ kind: "decoded", json });
        break;
      }
      case "closeStateCodec":
        this.sessions.delete(request.sessionId!);
        reply(null);
        break;
      default:
        throw new Error(`Unexpected worker request: ${request.type}`);
    }
  }

  terminate(): void {
    this.terminated = true;
  }
}

type Envelope = { type: string; data?: Record<string, unknown> };
type SocketListener = EventListenerOrEventListenerObject
  | ((event: CloseEvent) => void)
  | ((event: MessageEvent<string>) => void)
  | null;

class ControlledWebSocket extends EventTarget {
  static OPEN = 1;
  static instances: ControlledWebSocket[] = [];
  readyState = ControlledWebSocket.OPEN;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent<string>) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;
  readonly sent: Envelope[] = [];
  private readonly listenerWrappers = new Map<SocketListener, EventListener>();

  constructor(readonly url: string) {
    super();
    ControlledWebSocket.instances.push(this);
  }

  override addEventListener(type: string, listener: SocketListener, options?: AddEventListenerOptions | boolean): void {
    if (typeof listener !== "function") {
      super.addEventListener(type, listener, options);
      return;
    }
    let wrapper = this.listenerWrappers.get(listener);
    if (!wrapper) {
      wrapper = (event) => Reflect.apply(listener, this, [event]);
      this.listenerWrappers.set(listener, wrapper);
    }
    super.addEventListener(type, wrapper, options);
  }

  override removeEventListener(type: string, listener: SocketListener, options?: EventListenerOptions | boolean): void {
    if (typeof listener !== "function") {
      super.removeEventListener(type, listener, options);
      return;
    }
    const wrapper = this.listenerWrappers.get(listener);
    if (wrapper) super.removeEventListener(type, wrapper, options);
  }

  send(text: string): void {
    this.sent.push(JSON.parse(text) as Envelope);
  }

  receive(envelope: unknown): void {
    this.onmessage?.(new MessageEvent("message", { data: JSON.stringify(envelope) }));
  }

  close(): void {
    this.readyState = 3;
  }
}

const FULL_KEY = { game_code: "GAME01", generation: 1 };
const SERVER_HELLO = {
  type: "ServerHello",
  data: {
    server_version: "0.0.0-test", build_commit: "testhash", protocol_version: PROTOCOL_VERSION,
    mode: "Full", command_correlation: "CommandIdV1", state_codecs: ["StateCodecV1"],
  },
};
const CODEC_CONFIRMATION = {
  type: "ServerHello",
  data: { server_version: "0.0.0-test", selected_state_codec: "StateCodecV1" },
};
const DECK = { main_deck: [], sideboard: [] };
const SETTINGS = {
  displayName: "Alice", setCodes: ["MKM"], kind: "Premier" as const, public: true,
  tournamentFormat: "Swiss" as const, podPolicy: "Competitive" as const, podSize: 8,
};
const adapters: (WebSocketAdapter | ServerDraftAdapter)[] = [];

type RemoteKind = "game" | "draft";
type Attachment = {
  kind: RemoteKind;
  adapter: WebSocketAdapter | ServerDraftAdapter;
  completion: Promise<void>;
  setupType: string;
};

function gameState(turn: number): GameState {
  return {
    turn_number: turn, active_player: 0, phase: "PreCombatMain", players: [], priority_player: 0,
    objects: {}, next_object_id: 1, battlefield: [], stack: [], exile: [], rng_seed: 42,
    combat: null, waiting_for: { type: "Priority", data: { player: 0 } }, has_pending_cast: false,
    lands_played_this_turn: 0, max_lands_per_turn: 1, priority_pass_count: 0,
    pending_replacement: null, layers_dirty: false, next_timestamp: 1,
  };
}

function startGame(options: WebSocketAdapterOptions = {}): Attachment {
  const adapter = new WebSocketAdapter(
    "wss://phase.example/ws", "join", DECK, FULL_KEY.game_code,
    undefined, undefined, undefined, options,
  );
  adapters.push(adapter);
  const completion = adapter.initialize();
  void completion.catch(() => undefined);
  return { kind: "game", adapter, completion, setupType: options.nativeAi ? "CreateGameWithSettings" : "JoinGameWithPassword" };
}

function startAttachment(kind: RemoteKind, provider?: StateCodecSessionProvider | null): Attachment {
  const options = provider === undefined ? {} : { stateCodecSessionProvider: provider };
  if (kind === "game") return startGame(options);
  const adapter = new ServerDraftAdapter("wss://phase.example/ws", options);
  adapters.push(adapter);
  const completion = adapter.createDraft(SETTINGS);
  void completion.catch(() => undefined);
  return { kind, adapter, completion, setupType: "CreateDraftWithSettings" };
}

async function handshake(attachment: Attachment, socket: ControlledWebSocket, codec: boolean): Promise<void> {
  socket.receive(SERVER_HELLO);
  await drain();
  const hellos = socket.sent.filter((frame) => frame.type === "ClientHello");
  expect(hellos).toHaveLength(1);
  if (codec) expect(hellos[0].data?.state_codecs).toEqual(["StateCodecV1"]);
  else expect(hellos[0].data).not.toHaveProperty("state_codecs");
  expect(socket.sent.map((frame) => frame.type)).toContain(attachment.setupType);
  if (codec) socket.receive(CODEC_CONFIRMATION);
  await drain();
}

async function finishSnapshot(attachment: Attachment, socket: ControlledWebSocket, codec: boolean, turn = 1): Promise<void> {
  if (attachment.kind === "draft") {
    socket.receive({ type: "DraftCreated", data: { draft_code: "DRAFT1", player_token: "draft-token", seat_index: 0 } });
    await attachment.completion;
    socket.receive({ type: "DraftMatchStart", data: {
      match_id: "r1-t0", round: 1, game_code: FULL_KEY.game_code, full_key: FULL_KEY,
      player_token: "match-token", your_player: 0, opponent_name: "Bob",
    } });
  }
  const started = { type: "GameStarted", data: {
    state: gameState(turn), state_revision: turn, your_player: 0,
    player_token: "game-token", full_key: FULL_KEY, legal_actions: [{ type: "PassPriority" }],
  } };
  if (codec) {
    const worker = ControlledEngineWorker.instances[0];
    worker.decodedReplies.push(JSON.stringify(started));
    socket.receive({ type: "Full", data: { state: started, revision: turn } });
  } else socket.receive(started);
  await drain();
  await attachment.completion;
  expect((await attachment.adapter.getSnapshot()).state.turn_number).toBe(turn);
  expect((await attachment.adapter.getLegalActions()).actions).toEqual([{ type: "PassPriority" }]);
  expect(attachment.adapter.playerId).toBe(0);
}

beforeEach(() => {
  vi.useFakeTimers();
  startup = deferred<void>();
  ControlledEngineWorker.instances = [];
  ControlledWebSocket.instances = [];
  vi.stubGlobal("Worker", ControlledEngineWorker);
  vi.stubGlobal("WebSocket", ControlledWebSocket);
});

afterEach(async () => {
  for (const adapter of adapters.splice(0)) adapter.dispose();
  startup.resolve();
  await drain();
  getSharedAdapter().dispose();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("remote default codec startup", () => {
  it.each(["game", "draft"] as const)(
    "opens a cold default %s immediately with snapshots and negotiates only on a later warm attachment",
    async (kind) => {
      const cold = startAttachment(kind);
      await drain();
      // No clock advancement: an indefinitely held worker must not delay the socket.
      expect(ControlledWebSocket.instances).toHaveLength(1);
      expect(ControlledEngineWorker.instances).toHaveLength(1);
      const socket = ControlledWebSocket.instances[0];
      const worker = ControlledEngineWorker.instances[0];
      await handshake(cold, socket, false);
      await finishSnapshot(cold, socket, false);
      expect(worker.sessions.size).toBe(0);
      expect(worker.decodeCount).toBe(0);

      startup.resolve();
      await getSharedAdapter().initialize();
      await drain();
      // Warming must neither create a late unused lease nor upgrade the live socket.
      expect(worker.sessions.size).toBe(0);
      socket.receive({ type: "StateUpdate", data: {
        state: gameState(2), state_revision: 2, events: [], full_key: FULL_KEY,
      } });
      await drain();
      expect((await cold.adapter.getSnapshot()).state.turn_number).toBe(2);
      expect(socket.sent.filter((frame) => frame.type === "ClientHello")).toHaveLength(1);
      expect(worker.decodeCount).toBe(0);

      const warm = startAttachment(kind);
      await drain();
      expect(ControlledWebSocket.instances).toHaveLength(2);
      const later = ControlledWebSocket.instances[1];
      await handshake(warm, later, true);
      await finishSnapshot(warm, later, true, 3);
      expect(ControlledEngineWorker.instances).toHaveLength(1);
      expect(worker.sessions.size).toBe(1);
      expect(worker.decodeCount).toBe(1);
      warm.adapter.dispose();
      await drain();
      expect(worker.sessions.size).toBe(0);
      expect((await cold.adapter.getSnapshot()).state.turn_number).toBe(2);
    },
  );

  it.each([
    ["game", "ready"], ["draft", "ready"], ["game", "timeout"], ["draft", "timeout"],
  ] as const)(
    "keeps explicit %s providers waiting until startup is %s or the existing preparation bound expires",
    async (kind, outcome) => {
      const attachment = startAttachment(kind, () => getSharedAdapter().openStateCodecSession());
      await drain();
      expect(ControlledEngineWorker.instances).toHaveLength(1);
      expect(ControlledWebSocket.instances).toHaveLength(0);
      await vi.advanceTimersByTimeAsync(4999);
      expect(ControlledWebSocket.instances).toHaveLength(0);
      if (outcome === "ready") startup.resolve();
      else await vi.advanceTimersByTimeAsync(1);
      await drain();
      expect(ControlledWebSocket.instances).toHaveLength(1);
      const socket = ControlledWebSocket.instances[0];
      await handshake(attachment, socket, outcome === "ready");
      await finishSnapshot(attachment, socket, outcome === "ready");
      if (outcome === "timeout") {
        startup.resolve();
        await getSharedAdapter().initialize();
        await drain();
        expect(ControlledEngineWorker.instances[0].sessions.size).toBe(0);
        expect(ControlledEngineWorker.instances[0].decodeCount).toBe(0);
        expect(ControlledWebSocket.instances).toHaveLength(1);
      }
    },
  );

  it.each(["game null", "draft null", "native AI", "native P2P"] as const)(
    "keeps %s snapshot-only without warming the default runtime",
    async (mode) => {
      const socketFactory = (url: string) => new ControlledWebSocket(url);
      const attachment = mode === "native AI"
        ? startGame({ nativeAi: { socketFactory, aiSeats: [], playerCount: 2 } })
        : mode === "native P2P"
          ? startGame({ nativePregame: { kind: "guest", socketFactory } })
          : startAttachment(mode === "game null" ? "game" : "draft", null);
      await drain();
      expect(ControlledWebSocket.instances).toHaveLength(1);
      const socket = ControlledWebSocket.instances[0];
      await handshake(attachment, socket, false);
      await finishSnapshot(attachment, socket, false);
      expect(ControlledEngineWorker.instances).toHaveLength(0);
    },
  );
});
