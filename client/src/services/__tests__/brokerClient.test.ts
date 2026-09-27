import { beforeEach, describe, expect, it, vi } from "vitest";

import type { PhaseSocket } from "../openPhaseSocket";
import {
  BrokerRequestError,
  LobbyCapabilityError,
  lookupJoinTargetOver,
  makeBrokerClient,
  resolveGuestOver,
  subscribeLobbyOver,
} from "../brokerClient";
import type { RegisterHostRequest } from "../brokerClient";
import type { LobbyGame } from "../../adapter/types";
import {
  MIN_LOBBY_PROTOCOL_FOR_FREEFORM_FORMATS,
  PROTOCOL_VERSION,
  type ServerInfo,
} from "../../adapter/ws-adapter";
import { formatMetadata } from "../../data/formatRegistry";

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

  it.each([
    {
      label: "no format_config",
      frame: {
        game_code: "ABC123",
        is_p2p: true,
        player_count: 4,
        filled_seats: 1,
        match_config: { match_type: "Bo1" },
        draft_metadata: { setCode: "MKM", draftKind: "Premier" },
      },
    },
    {
      label: "a malformed format_config",
      frame: {
        game_code: "ABC123",
        is_p2p: true,
        player_count: 4,
        filled_seats: 1,
        match_config: { match_type: "Bo1" },
        format_config: { format: 42 },
        draft_metadata: { setCode: "MKM", draftKind: "Premier" },
      },
    },
  ])("carries draft_metadata through ($label)", async ({ frame }) => {
    const ws = new MockWebSocket();
    const promise = lookupJoinTargetOver(makePhaseSocket(ws), "ABC123");
    ws.deliver(JSON.stringify({ type: "JoinTargetInfo", data: frame }));

    const result = await promise;
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable: reach guard above");
    expect(result.info.draft_metadata).toEqual(frame.draft_metadata);
    // Reach guard for `withValidatedFormatConfig`'s rebuild branch: a
    // malformed `format_config` is dropped to `null` rather than passed
    // through, so this only discriminates when the frame supplied one.
    if ("format_config" in frame) {
      expect(result.info.format_config).toBeNull();
    }
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

describe("broker host registration privacy", () => {
  it("keeps deck and AI metadata out of LobbyOnly registration", async () => {
    const ws = new MockWebSocket();
    const client = makeBrokerClient(makePhaseSocket(ws));
    const request: RegisterHostRequest = {
      hostPeerId: "peer-host",
      displayName: "Host",
      public: true,
      password: null,
      timerSeconds: null,
      playerCount: 2,
      matchConfig: { match_type: "Bo1" },
      formatConfig: null,
      roomName: null,
      draftMetadata: null,
    };

    const registration = client.registerHost(request);
    const frame = JSON.parse(ws.send.mock.calls[0][0] as string) as {
      type: string;
      data: {
        host_peer_id: string;
        deck: Record<string, unknown>;
        ai_seats: unknown[];
      };
    };

    expect(frame.type).toBe("CreateGameWithSettings");
    expect(frame.data.host_peer_id).toBe("peer-host");
    expect(frame.data.deck).toEqual({
      main_deck: [],
      sideboard: [],
      commander: [],
      planar_deck: [],
      scheme_deck: [],
    });
    expect(frame.data.ai_seats).toEqual([]);
    expect(JSON.stringify(frame)).not.toContain("private-card");

    ws.deliver(
      JSON.stringify({
        type: "GameCreated",
        data: { game_code: "ABC123", player_token: "token" },
      }),
    );
    await expect(registration).resolves.toEqual({
      gameCode: "ABC123",
      playerToken: "token",
    });
  });
});

describe("registerHost lobby-capability floor", () => {
  function baseRequest(
    formatConfig: RegisterHostRequest["formatConfig"],
  ): RegisterHostRequest {
    return {
      hostPeerId: "peer-host",
      displayName: "Host",
      public: true,
      password: null,
      timerSeconds: null,
      playerCount: 2,
      matchConfig: { match_type: "Bo1" },
      formatConfig,
      roomName: null,
      draftMetadata: null,
    };
  }

  it.each([
    ["Freeform", 9],
    ["FreeformCommander", 9],
  ] as const)("rejects %s against lobby protocol %i without sending", async (name, lobbyProtocolVersion) => {
    const ws = new MockWebSocket();
    const client = makeBrokerClient(
      makePhaseSocket(ws, { lobbyProtocolVersion }),
    );
    const request = baseRequest(formatMetadata(name)!.default_config);

    await expect(client.registerHost(request)).rejects.toEqual(
      expect.objectContaining({
        neededLobbyVersion: MIN_LOBBY_PROTOCOL_FOR_FREEFORM_FORMATS,
      }),
    );
    const err = await client.registerHost(request).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(LobbyCapabilityError);
    expect(ws.send).not.toHaveBeenCalled();
  });

  it("fails closed when the broker advertises no lobby protocol version", async () => {
    const ws = new MockWebSocket();
    const client = makeBrokerClient(makePhaseSocket(ws, { lobbyProtocolVersion: undefined }));
    const request = baseRequest(formatMetadata("Freeform")!.default_config);

    await expect(client.registerHost(request)).rejects.toBeInstanceOf(LobbyCapabilityError);
    expect(ws.send).not.toHaveBeenCalled();
  });

  it("sends once the broker is at the floor (reach guard)", async () => {
    const ws = new MockWebSocket();
    const client = makeBrokerClient(
      makePhaseSocket(ws, { lobbyProtocolVersion: MIN_LOBBY_PROTOCOL_FOR_FREEFORM_FORMATS }),
    );
    const request = baseRequest(formatMetadata("Freeform")!.default_config);

    void client.registerHost(request);

    expect(ws.send).toHaveBeenCalledTimes(1);
    const frame = JSON.parse(ws.send.mock.calls[0][0] as string) as {
      type: string;
      data: { format_config: { format: string } };
    };
    expect(frame.type).toBe("CreateGameWithSettings");
    expect(frame.data.format_config.format).toBe("Freeform");
  });

  it("sends when formatConfig is null (nothing to gate on)", async () => {
    const ws = new MockWebSocket();
    const client = makeBrokerClient(makePhaseSocket(ws, { lobbyProtocolVersion: 9 }));
    const request = baseRequest(null);

    void client.registerHost(request);

    expect(ws.send).toHaveBeenCalledTimes(1);
  });

  it("sends a long-standing format name regardless of lobby version (name-driven gate)", async () => {
    const ws = new MockWebSocket();
    const client = makeBrokerClient(makePhaseSocket(ws, { lobbyProtocolVersion: 9 }));
    const request = baseRequest(formatMetadata("Commander")!.default_config);

    void client.registerHost(request);

    expect(ws.send).toHaveBeenCalledTimes(1);
  });

  it("sends a Custom: format regardless of lobby version", async () => {
    const ws = new MockWebSocket();
    const client = makeBrokerClient(makePhaseSocket(ws, { lobbyProtocolVersion: 9 }));
    const request = baseRequest({
      ...formatMetadata("Standard")!.default_config,
      format: "Custom:5",
    } as RegisterHostRequest["formatConfig"]);

    void client.registerHost(request);

    expect(ws.send).toHaveBeenCalledTimes(1);
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

describe("requested game codes (lobby protocol 10)", () => {
  const baseRequest: RegisterHostRequest = {
    hostPeerId: "peer-host",
    displayName: "Host",
    public: false,
    password: null,
    timerSeconds: null,
    playerCount: 4,
    matchConfig: { match_type: "Bo1" },
    formatConfig: null,
    roomName: null,
    draftMetadata: null,
  };

  function sentFrame(ws: MockWebSocket): { type: string; data: { requested_code?: unknown } } {
    return JSON.parse(ws.send.mock.calls[0][0] as string) as {
      type: string;
      data: { requested_code?: unknown };
    };
  }

  it("sends the requested code in the registration frame", () => {
    const ws = new MockWebSocket();
    const client = makeBrokerClient(makePhaseSocket(ws));
    void client.registerHost({ ...baseRequest, requestedCode: "AB12CD" });

    expect(sentFrame(ws).data.requested_code).toBe("AB12CD");
  });

  it("sends a null requested code when none is requested", () => {
    const ws = new MockWebSocket();
    const client = makeBrokerClient(makePhaseSocket(ws));
    void client.registerHost(baseRequest);

    expect(sentFrame(ws).data.requested_code).toBeNull();
  });

  it("rejects a held code with a typed BrokerRequestError", async () => {
    const ws = new MockWebSocket();
    const client = makeBrokerClient(makePhaseSocket(ws));
    const registration = client.registerHost({ ...baseRequest, requestedCode: "AB12CD" });
    ws.deliver(
      JSON.stringify({
        type: "Error",
        data: { message: "Game code AB12CD is already in use", code: "code_in_use" },
      }),
    );

    const err = await registration.catch((e: unknown) => e);
    expect(err).toBeInstanceOf(BrokerRequestError);
    expect((err as BrokerRequestError).code).toBe("code_in_use");
    expect((err as BrokerRequestError).message).toBe("Game code AB12CD is already in use");
  });

  it("still resolves GameCreated for a requested code", async () => {
    const ws = new MockWebSocket();
    const client = makeBrokerClient(makePhaseSocket(ws));
    const registration = client.registerHost({ ...baseRequest, requestedCode: "AB12CD" });
    ws.deliver(
      JSON.stringify({
        type: "GameCreated",
        data: { game_code: "AB12CD", player_token: "token" },
      }),
    );

    await expect(registration).resolves.toEqual({ gameCode: "AB12CD", playerToken: "token" });
  });

  it("classifies a typed game_not_found as not_found whatever the message", async () => {
    const ws = new MockWebSocket();
    const promise = lookupJoinTargetOver(makePhaseSocket(ws), "AB12CD");
    ws.deliver(
      JSON.stringify({
        type: "Error",
        data: { message: "No such room", code: "game_not_found" },
      }),
    );

    await expect(promise).resolves.toEqual(
      expect.objectContaining({ ok: false, reason: "not_found" }),
    );
  });

  it("still classifies a legacy un-coded not-found message", async () => {
    const ws = new MockWebSocket();
    const promise = lookupJoinTargetOver(makePhaseSocket(ws), "AB12CD");
    ws.deliver(
      JSON.stringify({
        type: "Error",
        data: { message: "Game not found in lobby: AB12CD" },
      }),
    );

    await expect(promise).resolves.toEqual(
      expect.objectContaining({ ok: false, reason: "not_found" }),
    );
  });
});
