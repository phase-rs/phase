import { beforeEach, describe, expect, it, vi } from "vitest";

import { WebSocketAdapter } from "../ws-adapter";
import type { GameState } from "../types";

// Minimal mock WebSocket
class MockWebSocket {
  static OPEN = 1;
  readyState = MockWebSocket.OPEN;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  send = vi.fn();
  close = vi.fn();
}

// Replace global WebSocket with mock
vi.stubGlobal("WebSocket", MockWebSocket);

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

  beforeEach(() => {
    adapter = new WebSocketAdapter(
      "ws://localhost:9374/ws",
      "host",
      { main_deck: [], sideboard: [] },
    );
    // Start initialize to trigger WS creation
    const initPromise = adapter.initialize();
    // Grab the created WS instance
    ws = (adapter as unknown as { ws: MockWebSocket }).ws;
    // Fire onopen to proceed with the protocol
    ws.onopen?.();
    // Simulate GameStarted to resolve init
    ws.onmessage?.({
      data: JSON.stringify({
        type: "GameStarted",
        data: { state: createMockState(), your_player: 0 },
      }),
    });
    return initPromise;
  });

  describe("Bug C: stateChanged emission", () => {
    it("emits stateChanged event when StateUpdate arrives without pendingResolve", () => {
      const listener = vi.fn();
      adapter.onEvent(listener);

      const mockState = createMockState();
      const mockEvents = [{ type: "DrawCard", data: { player: 0, object_id: 1 } }];

      // Simulate an unsolicited StateUpdate (no pending action)
      ws.onmessage?.({
        data: JSON.stringify({
          type: "StateUpdate",
          data: { state: mockState, events: mockEvents },
        }),
      });

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
    it("emits playerIdentity when GameStarted arrives", () => {
      const adapter2 = new WebSocketAdapter(
        "ws://localhost:9374/ws",
        "join",
        { main_deck: [], sideboard: [] },
        "ABC123",
      );
      const listener = vi.fn();
      adapter2.onEvent(listener);
      const initPromise2 = adapter2.initialize();
      const ws2 = (adapter2 as unknown as { ws: MockWebSocket }).ws;
      ws2.onopen?.();
      ws2.onmessage?.({
        data: JSON.stringify({
          type: "GameStarted",
          data: { state: createMockState(), your_player: 1, opponent_name: "Opponent" },
        }),
      });

      return initPromise2.then(() => {
        expect(listener).toHaveBeenCalledWith({
          type: "playerIdentity",
          playerId: 1,
          opponentName: "Opponent",
        });
      });
    });
  });

  describe("reconnect flow", () => {
    it("reconnects with the persisted session after socket close", async () => {
      vi.useFakeTimers();
      try {
        const reconnectingAdapter = new WebSocketAdapter(
          "ws://localhost:9374/ws",
          "join",
          { main_deck: [], sideboard: [] },
          "ABC123",
        );
        const initPromise = reconnectingAdapter.initialize();
        const initialWs = (reconnectingAdapter as unknown as { ws: MockWebSocket }).ws;
        initialWs.onopen?.();
        initialWs.onmessage?.({
          data: JSON.stringify({
            type: "GameStarted",
            data: {
              state: createMockState(),
              your_player: 1,
              player_token: "player-token",
            },
          }),
        });
        await initPromise;

        initialWs.onclose?.();
        await vi.advanceTimersByTimeAsync(1000);

        const reconnectWs = (reconnectingAdapter as unknown as { ws: MockWebSocket }).ws;
        reconnectWs.onopen?.();

        // After the version handshake was added, the first frame on open
        // is always ClientHello; the Reconnect frame is deferred until the
        // server's ServerHello arrives.
        expect(reconnectWs.send).toHaveBeenNthCalledWith(
          1,
          expect.stringContaining('"type":"ClientHello"'),
        );
        reconnectWs.onmessage?.({
          data: JSON.stringify({
            type: "ServerHello",
            data: {
              server_version: "0.0.0-test",
              build_commit: "testhash",
              protocol_version: 1,
              mode: "Full",
            },
          }),
        });
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
