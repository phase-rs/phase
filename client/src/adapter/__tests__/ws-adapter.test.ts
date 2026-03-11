import { beforeEach, describe, expect, it, vi } from "vitest";

import { WebSocketAdapter } from "../ws-adapter";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
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

// Mock sessionStorage
vi.stubGlobal("sessionStorage", {
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

  describe("Bug E: activePlayerId from GameStarted", () => {
    it("sets activePlayerId in multiplayerStore on GameStarted", () => {
      // Reset store
      useMultiplayerStore.getState().setActivePlayerId(null);

      // Create fresh adapter to trigger GameStarted again
      const adapter2 = new WebSocketAdapter(
        "ws://localhost:9374/ws",
        "join",
        { main_deck: [], sideboard: [] },
        "ABC123",
      );
      const initPromise2 = adapter2.initialize();
      const ws2 = (adapter2 as unknown as { ws: MockWebSocket }).ws;
      ws2.onopen?.();
      ws2.onmessage?.({
        data: JSON.stringify({
          type: "GameStarted",
          data: { state: createMockState(), your_player: 1 },
        }),
      });

      return initPromise2.then(() => {
        expect(useMultiplayerStore.getState().activePlayerId).toBe(1);
      });
    });
  });
});
