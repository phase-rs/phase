import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  NativeEngineVersionMismatchError,
  PROTOCOL_VERSION,
  WebSocketAdapter,
} from "../ws-adapter";
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
  // `openPhaseSocket` calls `addEventListener("close", ...)` / ("message", ...)
  // in addition to the legacy `onXxx` assignments. Route both channels:
  // legacy `onXxx` fires first, EventTarget listeners fire after.
  dispatchSynthetic(type: "message" | "close", data?: string) {
    if (type === "message" && data !== undefined) {
      this.onmessage?.({ data });
      this.dispatchEvent(new MessageEvent("message", { data }));
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

  describe("Bug D: getAiAction no-op", () => {
    it("getAiAction returns null without throwing", () => {
      const result = adapter.getAiAction("easy", 1);
      expect(result).toBeNull();
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
