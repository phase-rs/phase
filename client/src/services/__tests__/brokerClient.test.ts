import { beforeEach, describe, expect, it, vi } from "vitest";

import type { PhaseSocket } from "../openPhaseSocket";
import {
  lookupJoinTargetOver,
  makeBrokerClient,
  resolveGuestOver,
  subscribeLobbyOver,
} from "../brokerClient";
import type { LobbyGame } from "../../adapter/types";
import { PROTOCOL_VERSION, type ServerInfo } from "../../adapter/ws-adapter";

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

function makePhaseSocket(
  ws: MockWebSocket,
  serverInfo: Partial<ServerInfo> = {},
): PhaseSocket {
  return {
    ws: ws as unknown as WebSocket,
    serverInfo: {
      version: "0.0.0",
      buildCommit: "test",
      protocolVersion: 1,
      mode: "LobbyOnly",
      ...serverInfo,
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

describe("resolveGuestOver full-game surface guard", () => {
  // This resolver asks for `PeerInfo`. A `Full` server never publishes a P2P
  // row — both of its lobby registrations hardcode `host_peer_id:
  // String::new()` — so it has no peer id to return, and it answers this frame
  // off its server-run join path instead: `SessionAttached` + `StateUpdate`,
  // neither of which the listener handles. Sending would seat the guest
  // server-side and then time out as `connection_lost`. Refusing is
  // unconditional on mode so a relaxed lobby handshake can never carry a
  // full-game join.
  it.each([
    ["version-mismatched", PROTOCOL_VERSION - 2],
    ["version-compatible", PROTOCOL_VERSION],
  ])("refuses to send to a %s Full server", async (_label, protocolVersion) => {
    const ws = new MockWebSocket();
    const socket = makePhaseSocket(ws, { mode: "Full", protocolVersion });

    const result = await resolveGuestOver(socket, "ABC123");

    expect(result.ok).toBe(false);
    // The assertion that matters: nothing reached the wire, so no session can
    // have been attached and no game state can have been streamed back.
    expect(ws.send).not.toHaveBeenCalled();
  });

  it("still sends to a LobbyOnly broker whose full-game protocol is stale", async () => {
    // The broker cannot run a game, so its full-game number says nothing about
    // this frame and it is the one server kind that can answer with `PeerInfo`.
    // Guarding it would break the P2P join path this PR exists to keep working.
    const ws = new MockWebSocket();
    const socket = makePhaseSocket(ws, {
      mode: "LobbyOnly",
      protocolVersion: PROTOCOL_VERSION - 9,
    });

    const result = resolveGuestOver(socket, "ABC123");

    expect(ws.send).toHaveBeenCalledWith(
      expect.stringContaining('"type":"JoinGameWithPassword"'),
    );

    ws.fireClose();
    await expect(result).resolves.toMatchObject({
      ok: false,
      reason: "connection_lost",
    });
  });
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

  it("defaults deck-selection lookups to non-reserving metadata reads", async () => {
    const ws = new MockWebSocket();
    const promise = lookupJoinTargetOver(makePhaseSocket(ws), "ABC123");

    expect(ws.send).toHaveBeenCalledWith(
      JSON.stringify({
        type: "LookupJoinTarget",
        data: {
          game_code: "ABC123",
          password: null,
          reserve: false,
          display_name: null,
          release_reservation_token: null,
        },
      }),
    );

    ws.deliver(
      JSON.stringify({
        type: "JoinTargetInfo",
        data: {
          game_code: "ABC123",
          is_p2p: false,
          player_count: 2,
          filled_seats: 1,
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

describe("broker client keepalive", () => {
  function pingFrames(ws: MockWebSocket): { type: string }[] {
    return ws.send.mock.calls
      .map((call) => JSON.parse(call[0] as string) as { type: string })
      .filter((frame) => frame.type === "Ping");
  }

  it("pings its socket and stops when the socket closes", async () => {
    vi.useFakeTimers();
    try {
      const ws = new MockWebSocket();
      makeBrokerClient(makePhaseSocket(ws));

      await vi.advanceTimersByTimeAsync(11_000);
      expect(pingFrames(ws).length).toBeGreaterThanOrEqual(2);

      ws.fireClose();
      ws.send.mockClear();
      await vi.advanceTimersByTimeAsync(11_000);
      expect(pingFrames(ws)).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("stops pinging when the client is closed", async () => {
    vi.useFakeTimers();
    try {
      const ws = new MockWebSocket();
      const client = makeBrokerClient(makePhaseSocket(ws));

      await vi.advanceTimersByTimeAsync(11_000);
      // Reach guard: the interval was running before `close()` ended it.
      expect(pingFrames(ws).length).toBeGreaterThanOrEqual(2);

      client.close();
      ws.send.mockClear();
      await vi.advanceTimersByTimeAsync(11_000);
      expect(pingFrames(ws)).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });
});
