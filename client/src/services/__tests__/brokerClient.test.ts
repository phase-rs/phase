import { beforeEach, describe, expect, it, vi } from "vitest";

import type { PhaseSocket } from "../openPhaseSocket";
import {
  lookupJoinTargetOver,
  resolveGuestOver,
  subscribeLobbyOver,
} from "../brokerClient";
import type { LobbyGame } from "../../adapter/types";

class MockWebSocket extends EventTarget {
  static OPEN = 1;
  readyState = MockWebSocket.OPEN;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  send = vi.fn();
  close = vi.fn();

  deliver(data: string) {
    this.onmessage?.({ data });
    this.dispatchEvent(new MessageEvent("message", { data }));
  }
  fireClose() {
    this.onclose?.();
    this.dispatchEvent(new Event("close"));
  }
}

function makePhaseSocket(ws: MockWebSocket): PhaseSocket {
  return {
    ws: ws as unknown as WebSocket,
    serverInfo: {
      version: "0.0.0",
      buildCommit: "test",
      protocolVersion: 1,
      mode: "LobbyOnly",
    },
    close: () => ws.close(),
  };
}

beforeEach(() => {
  // Some JSDOM envs don't implement MessageEvent — polyfill minimally.
  if (typeof MessageEvent === "undefined") {
    vi.stubGlobal("MessageEvent", class {
      constructor(public type: string, public init: { data: string }) {}
      get data() {
        return this.init.data;
      }
    });
  }
});

describe("resolveGuestOver", () => {
  it("resolves with peerInfo on PeerInfo frame for the matching code", async () => {
    const ws = new MockWebSocket();
    const socket = makePhaseSocket(ws);
    const promise = resolveGuestOver(socket, "ABC123");
    ws.deliver(
      JSON.stringify({
        type: "PeerInfo",
        data: {
          game_code: "ABC123",
          host_peer_id: "peer-xyz",
          player_count: 2,
          filled_seats: 1,
          match_config: { match_type: "Bo1" },
        },
      }),
    );
    const result = await promise;
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.peerInfo.host_peer_id).toBe("peer-xyz");
  });

  it("returns password_required on PasswordRequired frame", async () => {
    const ws = new MockWebSocket();
    const promise = resolveGuestOver(makePhaseSocket(ws), "ABC123");
    ws.deliver(
      JSON.stringify({
        type: "PasswordRequired",
        data: { game_code: "ABC123" },
      }),
    );
    const result = await promise;
    expect(result).toEqual(
      expect.objectContaining({ ok: false, reason: "password_required" }),
    );
  });

  it("classifies build-mismatch errors correctly", async () => {
    const ws = new MockWebSocket();
    const promise = resolveGuestOver(makePhaseSocket(ws), "ABC123");
    ws.deliver(
      JSON.stringify({
        type: "Error",
        data: { message: "Build mismatch: host is on X, you are on Y." },
      }),
    );
    const result = await promise;
    expect(result).toEqual(
      expect.objectContaining({ ok: false, reason: "build_mismatch" }),
    );
  });

  it("resolves connection_lost on socket close mid-flight", async () => {
    const ws = new MockWebSocket();
    const promise = resolveGuestOver(makePhaseSocket(ws), "ABC123");
    ws.fireClose();
    const result = await promise;
    expect(result).toEqual(
      expect.objectContaining({ ok: false, reason: "connection_lost" }),
    );
  });

  it("ignores PeerInfo for a different game code", async () => {
    const ws = new MockWebSocket();
    const promise = resolveGuestOver(makePhaseSocket(ws), "ABC123");
    ws.deliver(
      JSON.stringify({
        type: "PeerInfo",
        data: { game_code: "OTHER", host_peer_id: "wrong", player_count: 2, filled_seats: 0, match_config: { match_type: "Bo1" } },
      }),
    );
    // A stray frame for a different code shouldn't resolve our promise.
    // We assert via a race with a short timer instead of waiting
    // indefinitely.
    const raced = await Promise.race([
      promise,
      new Promise((r) => setTimeout(() => r("pending"), 20)),
    ]);
    expect(raced).toBe("pending");
  });
});

describe("lookupJoinTargetOver", () => {
  it("resolves with JoinTargetInfo for the matching code", async () => {
    const ws = new MockWebSocket();
    const promise = lookupJoinTargetOver(makePhaseSocket(ws), "ABC123");
    ws.deliver(
      JSON.stringify({
        type: "JoinTargetInfo",
        data: {
          game_code: "ABC123",
          is_p2p: false,
          player_count: 2,
          filled_seats: 1,
          match_config: { match_type: "Bo1" },
          format_config: { format: "Commander" },
        },
      }),
    );
    const result = await promise;
    expect(result).toEqual(
      expect.objectContaining({
        ok: true,
        info: expect.objectContaining({
          game_code: "ABC123",
          is_p2p: false,
        }),
      }),
    );
  });

  it("sends LookupJoinTarget instead of JoinGameWithPassword", async () => {
    const ws = new MockWebSocket();
    const promise = lookupJoinTargetOver(makePhaseSocket(ws), "ABC123", "pw");
    expect(ws.send).toHaveBeenCalledWith(
      expect.stringContaining('"type":"LookupJoinTarget"'),
    );
    expect(ws.send).toHaveBeenCalledWith(
      expect.stringContaining('"password":"pw"'),
    );
    ws.deliver(
      JSON.stringify({
        type: "JoinTargetInfo",
        data: {
          game_code: "ABC123",
          is_p2p: true,
          player_count: 4,
          filled_seats: 2,
          match_config: { match_type: "Bo1" },
        },
      }),
    );
    await promise;
  });
});

describe("subscribeLobbyOver", () => {
  it("sends SubscribeLobby on attach and dispatches snapshot + deltas to onUpdate", () => {
    const ws = new MockWebSocket();
    const updates: LobbyGame[][] = [];
    const unsub = subscribeLobbyOver(makePhaseSocket(ws), (games) =>
      updates.push(games),
    );

    expect(ws.send).toHaveBeenCalledWith(
      expect.stringContaining('"type":"SubscribeLobby"'),
    );

    ws.deliver(
      JSON.stringify({
        type: "LobbyUpdate",
        data: {
          games: [
            {
              game_code: "ONE",
              host_name: "A",
              created_at: 1,
              has_password: false,
              is_p2p: true,
            },
          ],
        },
      }),
    );
    ws.deliver(
      JSON.stringify({
        type: "LobbyGameAdded",
        data: {
          game: {
            game_code: "TWO",
            host_name: "B",
            created_at: 2,
            has_password: false,
          },
        },
      }),
    );
    ws.deliver(
      JSON.stringify({
        type: "LobbyGameRemoved",
        data: { game_code: "ONE" },
      }),
    );

    expect(updates).toHaveLength(3);
    expect(updates[0]).toHaveLength(1);
    expect(updates[1]).toHaveLength(2);
    expect(updates[2]).toEqual([
      expect.objectContaining({ game_code: "TWO" }),
    ]);

    unsub();
    expect(ws.send).toHaveBeenCalledWith(
      expect.stringContaining('"type":"UnsubscribeLobby"'),
    );
  });
});
