import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  NativeEngineVersionMismatchError,
  PROTOCOL_VERSION,
  WebSocketAdapter,
} from "../ws-adapter";
import { AdapterError, supportsMatchConcede } from "../types";
import type { GameState } from "../types";
import type { PhaseSocketTransport } from "../../services/openPhaseSocket";

// Minimal mock WebSocket. Latest-constructed instance is exposed via
// `MockWebSocket.last` so tests can grab it synchronously — the adapter
// now opens the socket through the async `openPhaseSocket` helper, so
// `adapter.ws` is not populated until after the handshake completes.
class MockWebSocket extends EventTarget {
  static OPEN = 1;
  static last: MockWebSocket | null = null;
  readyState = MockWebSocket.OPEN;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  send = vi.fn();
  close = vi.fn();
  constructor(public url: string) {
    super();
    MockWebSocket.last = this;
  }
  // Deliver a frame the way production does — which is asymmetric between
  // the two event types, so this routes them differently.
  //
  // `"message"`: `onmessage` ONLY. Both `openPhaseSocket`
  // (`openPhaseSocket.ts:190`) and the adapter (`ws-adapter.ts:528`) assign
  // the `onmessage` IDL attribute, and neither registers an
  // `addEventListener("message", ...)` — `PhaseSocketTransport` types
  // `addEventListener` for `"close"` alone (`openPhaseSocket.ts:20-25`), so
  // a message listener is not even expressible through the interface. This
  // previously also called `dispatchEvent(new MessageEvent(...))` under a
  // comment claiming `openPhaseSocket` registered a message listener; it
  // does not, and happy-dom routes `dispatchEvent` back through the
  // `onmessage` attribute, so every frame in this file was handled TWICE.
  // Harmless for idempotent handlers, but it silently defeats any negative
  // that depends on state the first delivery consumes.
  //
  // `"close"`: both channels, because production genuinely uses both —
  // `openPhaseSocket.ts:366` registers `addEventListener("close", ...)`
  // alongside the `onclose` assignment.
  dispatchSynthetic(type: "message" | "close", data?: string) {
    if (type === "message" && data !== undefined) {
      this.onmessage?.({ data });
    } else if (type === "close") {
      this.onclose?.();
      this.dispatchEvent(new Event("close"));
    }
  }
}

// Replace global WebSocket with mock
vi.stubGlobal("WebSocket", MockWebSocket);

const SERVER_HELLO = JSON.stringify({
  type: "ServerHello",
  data: {
    server_version: "0.0.0-test",
    build_commit: "testhash",
    protocol_version: PROTOCOL_VERSION,
    mode: "Full",
  },
});

/**
 * Drives an adapter through the shared-handshake pipeline to the
 * post-ServerHello state. Returns the adapter's underlying mock ws once
 * the handshake has landed, so tests can then fire game-level frames.
 */
async function completeHandshake(adapter: WebSocketAdapter): Promise<MockWebSocket> {
  // Allow the microtask inside `openPhaseSocket` to install its
  // `onmessage` handler before we deliver the hello frame.
  await Promise.resolve();
  const ws = MockWebSocket.last!;
  ws.dispatchSynthetic("message", SERVER_HELLO);
  // One more tick so the adapter's `attachSocket` re-binds `onmessage`
  // to its post-handshake handler and the `this.ws` assignment settles.
  await Promise.resolve();
  await Promise.resolve();
  return (adapter as unknown as { ws: MockWebSocket }).ws;
}

// Shared session service relies on localStorage in test environments.
vi.stubGlobal("localStorage", {
  getItem: vi.fn(() => null),
  setItem: vi.fn(),
  removeItem: vi.fn(),
});

function createMockState(): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [],
    priority_player: 0,
    objects: {},
    next_object_id: 1,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 42,
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
  };
}

describe("WebSocketAdapter", () => {
  let adapter: WebSocketAdapter;
  let ws: MockWebSocket;

  beforeEach(async () => {
    MockWebSocket.last = null;
    adapter = new WebSocketAdapter(
      "ws://localhost:9374/ws",
      "host",
      { main_deck: [], sideboard: [] },
    );
    const initPromise = adapter.initialize();
    ws = await completeHandshake(adapter);
    // Simulate GameStarted to resolve init.
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "GameStarted",
        data: { state: createMockState(), your_player: 0 },
      }),
    );
    await initPromise;
  });

  it("sends the payload-free authenticated whole-match concession intent", () => {
    expect(supportsMatchConcede(adapter)).toBe(true);

    adapter.sendMatchConcede();

    expect(ws.send).toHaveBeenLastCalledWith(JSON.stringify({ type: "ConcedeMatch" }));
  });

  describe("native AI transport", () => {
    const nativeAiOptions = (socketFactory: () => PhaseSocketTransport) => ({
      nativeAi: {
        socketFactory,
        aiSeats: [{
          seatIndex: 1,
          difficulty: "Hard",
          deck: { main_deck: ["Lightning Bolt"], sideboard: [] },
        }],
        playerCount: 2,
      },
    });

    it("uses the bridge factory with the full camelCase AI seat wire shape", async () => {
      MockWebSocket.last = null;
      const socketFactory = vi.fn(
        () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
      );
      const nativeAdapter = new WebSocketAdapter(
        "not-a-websocket-url",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Player",
        nativeAiOptions(socketFactory),
      );

      const initPromise = nativeAdapter.initialize();
      const nativeSocket = await completeHandshake(nativeAdapter);
      expect(socketFactory).toHaveBeenCalledWith("not-a-websocket-url");
      expect(nativeSocket.send).toHaveBeenLastCalledWith(
        JSON.stringify({
          type: "CreateGameWithSettings",
          data: {
            deck: { main_deck: [], sideboard: [] },
            display_name: "Player",
            public: false,
            password: null,
            timer_seconds: null,
            player_count: 2,
            match_config: { match_type: "Bo1" },
            ai_seats: [{
              seatIndex: 1,
              difficulty: "Hard",
              deckName: null,
              deck: {
                type: "DeckList",
                data: { main_deck: ["Lightning Bolt"], sideboard: [] },
              },
            }],
            format_config: null,
            room_name: null,
            start_when_full: true,
            ranked: false,
          },
        }),
      );
      const calls = nativeSocket.send.mock.calls;
      const sentFrame = calls[calls.length - 1]?.[0];
      expect(sentFrame).toContain('"seatIndex"');
      expect(sentFrame).toContain('"deckName"');
      expect(sentFrame).not.toContain('"seat_index"');
      expect(sentFrame).not.toContain('"deck_name"');
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: { state: createMockState(), your_player: 0 },
        }),
      );
      await initPromise;
    });

    // A dropped socket used to be instantly fatal for desktop solo:
    // `maxReconnectAttempts` was 0, and `attemptReconnect` compares
    // `reconnectAttempt >= maxReconnectAttempts`, so 0 >= 0 short-circuits to
    // `reconnectFailed` before the `reconnecting` emit at all. The sidecar
    // runs `--single-user`, so its reconnect window is effectively unbounded
    // and the session is still there to reconnect to.
    it("retries a dropped native-ai socket instead of failing on first drop", async () => {
      MockWebSocket.last = null;
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Player",
        nativeAiOptions(
          () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
        ),
      );

      const initPromise = nativeAdapter.initialize();
      await Promise.resolve();
      const nativeSocket = MockWebSocket.last!;
      nativeSocket.dispatchSynthetic("message", SERVER_HELLO);
      await Promise.resolve();
      await Promise.resolve();
      // A live session is the first branch `attemptReconnect` reaches: with no
      // game code / player token it short-circuits to `reconnectFailed`
      // regardless of the cap, so the fixture must establish one or the test
      // measures the wrong branch.
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameCreated",
          data: { game_code: "ABCD", player_token: "tok", full_key: { game_code: "ABCD", generation: 1 } },
        }),
      );
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: { state: createMockState(), your_player: 0 },
        }),
      );
      await initPromise;

      const listener = vi.fn();
      nativeAdapter.onEvent(listener);
      nativeSocket.dispatchSynthetic("close");

      // Presently unsatisfiable on the old code: `attemptReconnect` returned
      // before the `reconnecting` emit, so this event was NEVER produced for
      // a native-ai adapter.
      expect(listener).toHaveBeenCalledWith({
        type: "reconnecting",
        attempt: 1,
        maxAttempts: 8,
      });
      expect(listener).not.toHaveBeenCalledWith(
        expect.objectContaining({ type: "reconnectFailed" }),
      );

      nativeAdapter.dispose();
    });

    // Scope guard: this asserts a property of the DIFF (the change was scoped
    // to `nativeAi` and did not widen to both options), not that 0 is the
    // right answer for pregame — that path is explicitly not analysed.
    it("leaves the native pregame transport failing on first drop", async () => {
      MockWebSocket.last = null;
      const pregameAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 2,
            aiSeats: [],
          },
        },
      );

      const attached = pregameAdapter.initializePregame();
      const nativeSocket = await completeHandshake(pregameAdapter);
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "SessionAttached",
          data: { game_code: "WXYZ", player_id: 0, player_token: "tok", full_key: { game_code: "WXYZ", generation: 1 } },
        }),
      );
      await attached;

      const listener = vi.fn();
      pregameAdapter.onEvent(listener);
      nativeSocket.dispatchSynthetic("close");

      expect(listener).toHaveBeenCalledWith({ type: "reconnectFailed" });
      expect(listener).not.toHaveBeenCalledWith(
        expect.objectContaining({ type: "reconnecting" }),
      );

      pregameAdapter.dispose();
    });

    it("rejects a release version mismatch before creating a game", async () => {
      MockWebSocket.last = null;
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Player",
        {
          nativeAi: {
            ...nativeAiOptions(
              () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            ).nativeAi,
            expectedServerVersion: "1.2.3",
          },
        },
      );

      const initPromise = nativeAdapter.initialize();
      await Promise.resolve();
      const nativeSocket = MockWebSocket.last!;
      nativeSocket.dispatchSynthetic("message", SERVER_HELLO);

      await expect(initPromise).rejects.toBeInstanceOf(NativeEngineVersionMismatchError);
      expect(nativeSocket.close).toHaveBeenCalledOnce();
      expect(nativeSocket.send).not.toHaveBeenCalledWith(
        expect.stringContaining("CreateGameWithSettings"),
      );
    });
  });

  describe("native P2P pregame transport", () => {
    it("rejects a native seat attachment without a Full session key", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 2,
            aiSeats: [],
          },
        },
      );

      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "SessionAttached",
          data: { game_code: "NATIVE", player_id: 0, player_token: "host-token" },
        }),
      );

      await expect(attached).rejects.toMatchObject({ message: "Server omitted a valid Full session identity" });
      nativeAdapter.dispose();
    });

    it("waits for the server-issued seat attachment and slot confirmation", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 2,
            aiSeats: [],
          },
        },
      );

      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      expect(nativeSocket.send).toHaveBeenLastCalledWith(
        expect.stringContaining('"start_when_full":false'),
      );

      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "SessionAttached",
          data: { game_code: "NATIVE", player_id: 0, player_token: "host-token", full_key: { game_code: "NATIVE", generation: 1 } },
        }),
      );
      await expect(attached).resolves.toEqual({
        gameCode: "NATIVE",
        playerId: 0,
        playerToken: "host-token",
        fullKey: { game_code: "NATIVE", generation: 1 },
      });

      const confirmed = nativeAdapter.sendSeatMutation({ type: "Start" });
      expect(nativeSocket.send).toHaveBeenLastCalledWith(
        JSON.stringify({ type: "SeatMutate", data: { mutation: { type: "Start" } } }),
      );
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({ type: "PlayerSlotsUpdate", data: { slots: [] } }),
      );
      await expect(confirmed).resolves.toBeUndefined();
    });

    it("reconnects a persisted native viewer with its expected seat", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "join",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Guest",
        {
          nativePregame: {
            kind: "reconnect",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            gameCode: "NATIVE",
            playerId: 1,
            playerToken: "guest-token",
            fullKey: { game_code: "NATIVE", generation: 1 },
          },
        },
      );

      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      expect(nativeSocket.send).toHaveBeenLastCalledWith(
        JSON.stringify({
          type: "Reconnect",
          data: {
            game_code: "NATIVE",
            player_token: "guest-token",
            full_key: { game_code: "NATIVE", generation: 1 },
          },
        }),
      );
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: { state_revision: 7, state: createMockState(), your_player: 1 },
        }),
      );
      await expect(attached).resolves.toEqual({
        gameCode: "NATIVE",
        playerId: 1,
        playerToken: "guest-token",
        fullKey: { game_code: "NATIVE", generation: 1 },
      });
    });

    it("rejects native pregame attachment when the server returns an error", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 2,
            aiSeats: [],
          },
        },
      );

      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({ type: "Error", data: { message: "Native setup failed" } }),
      );

      await expect(attached).rejects.toThrow("Native setup failed");
    });

    it("rejects native lifecycle waiters when disposed before the socket is attached", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 2,
            aiSeats: [],
          },
        },
      );

      const attached = nativeAdapter.initializePregame();
      const gameStarted = nativeAdapter.waitForGameStarted();
      nativeAdapter.dispose();

      await expect(attached).rejects.toMatchObject({
        code: "WS_CLOSED",
        recoverable: true,
      } satisfies Partial<AdapterError>);
      await expect(gameStarted).rejects.toMatchObject({
        code: "WS_CLOSED",
        recoverable: true,
      } satisfies Partial<AdapterError>);
    });

    it("preserves the typed non-recoverable deck rejection code", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 2,
            aiSeats: [],
          },
        },
      );

      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "Error",
          data: { message: "Deck not legal for this format", code: "deck_rejected" },
        }),
      );

      await expect(attached).rejects.toMatchObject({
        code: "DECK_REJECTED",
        recoverable: false,
      } satisfies Partial<AdapterError>);
    });

    it("does not infer deck rejection from matching error text without a code", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 2,
            aiSeats: [],
          },
        },
      );

      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({ type: "Error", data: { message: "Deck not legal for this format" } }),
      );

      await expect(attached).rejects.toMatchObject({
        code: "ACTION_REJECTED",
        recoverable: true,
      } satisfies Partial<AdapterError>);
    });
  });

  describe("Bug C: stateChanged emission", () => {
    it("emits stateChanged event when StateUpdate arrives without pendingResolve", () => {
      const listener = vi.fn();
      adapter.onEvent(listener);

      const mockState = createMockState();
      const mockEvents = [{ type: "DrawCard", data: { player: 0, object_id: 1 } }];
      const mockLogEntries = [{
        seq: 0,
        turn: 1,
        phase: "PreCombatMain",
        category: "Debug",
        segments: [{ type: "Text", value: "AI guesses Land" }],
      }];

      // Simulate an unsolicited StateUpdate (no pending action)
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "StateUpdate",
          data: { state: mockState, events: mockEvents, log_entries: mockLogEntries },
        }),
      );

      // The engine pair now travels as one seq-stamped `EngineSnapshot`.
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({
          type: "stateChanged",
          snapshot: expect.objectContaining({
            state: expect.objectContaining(mockState),
            seq: expect.any(Number),
          }),
          events: mockEvents,
          logEntries: mockLogEntries,
        }),
      );
    });
  });

  describe("GameStarted identity event", () => {
    it("emits playerIdentity when GameStarted arrives", async () => {
      MockWebSocket.last = null;
      const adapter2 = new WebSocketAdapter(
        "ws://localhost:9374/ws",
        "join",
        { main_deck: [], sideboard: [] },
        "ABC123",
      );
      const listener = vi.fn();
      adapter2.onEvent(listener);
      const initPromise2 = adapter2.initialize();
      const ws2 = await completeHandshake(adapter2);
      ws2.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: { state: createMockState(), your_player: 1, opponent_name: "Opponent" },
        }),
      );
      await initPromise2;
      expect(listener).toHaveBeenCalledWith({
        type: "playerIdentity",
        playerId: 1,
        opponentName: "Opponent",
      });
    });
  });

  describe("reconnect flow", () => {
    it("reconnects with the persisted session after socket close", async () => {
      MockWebSocket.last = null;
      const reconnectingAdapter = new WebSocketAdapter(
        "ws://localhost:9374/ws",
        "join",
        { main_deck: [], sideboard: [] },
        "ABC123",
      );
      const initPromise = reconnectingAdapter.initialize();
      const initialWs = await completeHandshake(reconnectingAdapter);
      initialWs.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: {
            state: createMockState(),
            your_player: 1,
            player_token: "player-token",
            full_key: { game_code: "ABC123", generation: 1 },
          },
        }),
      );
      await initPromise;

      vi.useFakeTimers();
      try {
        initialWs.dispatchSynthetic("close");
        await vi.advanceTimersByTimeAsync(1000);
        vi.useRealTimers();

        const reconnectWs = await completeHandshake(reconnectingAdapter);

        // The handshake helper consumes ServerHello and sends ClientHello
        // internally, so after `completeHandshake` the first post-handshake
        // frame the adapter emits is the Reconnect setup frame.
        expect(reconnectWs.send).toHaveBeenCalledWith(
          JSON.stringify({
            type: "Reconnect",
            data: {
              game_code: "ABC123",
              player_token: "player-token",
              full_key: { game_code: "ABC123", generation: 1 },
            },
          }),
        );
      } finally {
        vi.useRealTimers();
      }
    });
  });

  describe("send() error handling", () => {
    it("rejects initialize when the post-handshake setup frame cannot be sent", async () => {
      MockWebSocket.last = null;
      const setupFailingAdapter = new WebSocketAdapter(
        "ws://localhost:9374/ws",
        "host",
        { main_deck: [], sideboard: [] },
      );
      const initPromise = setupFailingAdapter.initialize();
      await Promise.resolve();
      const setupWs = MockWebSocket.last!;
      setupWs.send
        .mockImplementationOnce(() => undefined)
        .mockImplementationOnce(() => {
          throw new Error("InvalidStateError");
        });

      setupWs.dispatchSynthetic("message", SERVER_HELLO);

      await expect(initPromise).rejects.toThrow("Failed to send setup frame");
    });

    // Issue #5913: the engine's stale-ReorderHand verdict must classify the same
    // way no matter which transport delivered it. Before the shared classifier
    // this path built a generic ACTION_REJECTED, so `dispatchAction` — which
    // suppresses only STALE_ACTION — still showed a server-hosted player the red
    // error the local-WASM seat no longer sees.
    it("classifies a stale ReorderHand rejection from the server as STALE_ACTION", async () => {
      const pending = adapter.submitAction(
        { type: "ReorderHand", data: { order: [1, 2, 3] } },
        0,
      );
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "ActionRejected",
          data: { reason: "Engine error: ReorderHand: expected 6 ids, got 5" },
        }),
      );
      await expect(pending).rejects.toMatchObject({
        code: "STALE_ACTION",
        recoverable: false,
      });
    });

    it("still surfaces a non-stale server rejection as a recoverable ACTION_REJECTED", async () => {
      const pending = adapter.submitAction({ type: "PassPriority" }, 0);
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "ActionRejected",
          data: { reason: "Engine error: Something genuinely wrong" },
        }),
      );
      await expect(pending).rejects.toMatchObject({
        code: "ACTION_REJECTED",
        recoverable: true,
      });
    });

    // A refused takeback answers a fire-and-forget request, so no promise owns
    // the rejection. Before this branch the whole `if (this.pendingReject)`
    // body was skipped and the refusal was dropped on the floor — which is why
    // the server had been reaching for `ServerMessage::error` instead, the
    // event `handleNativeEvent` treats as terminal.
    it("emits requestRejected when an ActionRejected has no in-flight action", () => {
      const listener = vi.fn();
      adapter.onEvent(listener);

      adapter.sendRequestTakeback();
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "ActionRejected",
          data: { reason: "There is no previous action of yours to take back" },
        }),
      );

      expect(listener).toHaveBeenCalledWith({
        type: "requestRejected",
        reason: "There is no previous action of yours to take back",
      });
      // The survivability property: no terminal `error` event, which is what
      // tears down a native session.
      expect(listener).not.toHaveBeenCalledWith(
        expect.objectContaining({ type: "error" }),
      );
    });

    // Guards the new `else` against swallowing the normal path.
    //
    // This test is why the double delivery in `dispatchSynthetic` had to be
    // fixed rather than worked around: a second delivery of the same frame
    // finds `pendingReject` already cleared by the first, takes the `else`,
    // and makes the negative below unpassable no matter what the adapter
    // does. It now goes through `dispatchSynthetic` like every other frame
    // in this file.
    it("does not emit requestRejected when an action IS in flight", async () => {
      const listener = vi.fn();
      adapter.onEvent(listener);

      const pending = adapter.submitAction({ type: "PassPriority" }, 0);
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "ActionRejected",
          data: { reason: "Engine error: Something genuinely wrong" },
        }),
      );

      // Reach-guard: the frame really was delivered and handled — without this
      // the negative below would pass for a message that never arrived.
      await expect(pending).rejects.toMatchObject({ code: "ACTION_REJECTED" });
      expect(listener).not.toHaveBeenCalledWith(
        expect.objectContaining({ type: "requestRejected" }),
      );
    });

    it("sends the action frame and keeps the promise pending on a healthy socket", () => {
      ws.send.mockClear();
      void adapter.submitAction({ type: "PassPriority" }, 0);
      expect(ws.send).toHaveBeenCalledWith(
        JSON.stringify({
          type: "Action",
          data: { action: { type: "PassPriority" } },
        }),
      );
    });

    it("resolves a mana-payment preview only for its matching request", async () => {
      ws.send.mockClear();
      const preview = adapter.previewManaPayment({ type: "PassPriority" }, 0);
      expect(ws.send).toHaveBeenCalledWith(
        JSON.stringify({
          type: "PreviewManaPayment",
          data: { request_id: 1, action: { type: "PassPriority" } },
        }),
      );

      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "ManaPaymentPreview",
          data: { request_id: 1, source_ids: [12] },
        }),
      );

      await expect(preview).resolves.toEqual([12]);
    });

    it("rejects submitAction and clears pending state when the socket throws on send", async () => {
      const listener = vi.fn();
      adapter.onEvent(listener);
      ws.send.mockImplementationOnce(() => {
        throw new Error("InvalidStateError");
      });

      await expect(
        adapter.submitAction({ type: "PassPriority" }, 0),
      ).rejects.toThrow();

      // The action was un-pended and an error surfaced, rather than the caller
      // hanging forever on a reply that will never come.
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({ type: "actionPendingChanged", pending: false }),
      );
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({ type: "error" }),
      );
    });

    it("emits an error instead of throwing when a fire-and-forget send hits a closed socket", () => {
      const listener = vi.fn();
      adapter.onEvent(listener);
      ws.readyState = 3; // CLOSED

      expect(() => adapter.sendEmote("wave")).not.toThrow();
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({ type: "error" }),
      );
    });
  });
});
