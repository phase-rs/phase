import { beforeEach, describe, expect, it, vi } from "vitest";

import { WebSocketAdapter } from "../ws-adapter";
import type { GameState } from "../types";

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
    protocol_version: 3,
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

  describe("Bug C: stateChanged emission", () => {
    it("emits stateChanged event when StateUpdate arrives without pendingResolve", () => {
      const listener = vi.fn();
      adapter.onEvent(listener);

      const mockState = createMockState();
      const mockEvents = [{ type: "DrawCard", data: { player: 0, object_id: 1 } }];

      // Simulate an unsolicited StateUpdate (no pending action)
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "StateUpdate",
          data: { state: mockState, events: mockEvents },
        }),
      );

      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({
          type: "stateChanged",
          state: mockState,
          events: mockEvents,
        }),
      );
    });
  });

  describe("Bug D: getAiAction no-op", () => {
    it("getAiAction returns null without throwing", () => {
      const result = adapter.getAiAction("easy");
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
});
