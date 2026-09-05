import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  HandshakeError,
  openPhaseSocket,
  withReconnect,
} from "../openPhaseSocket";
import {
  LOBBY_MIN_SUPPORTED_SERVER_PROTOCOL,
  LOBBY_PROTOCOL_VERSION,
  MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL,
  PROTOCOL_VERSION,
} from "../../adapter/ws-adapter";

class MockWebSocket extends EventTarget {
  static OPEN = 1;
  static instances: MockWebSocket[] = [];
  readyState = MockWebSocket.OPEN;
  binaryType: BinaryType = "blob";
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  send = vi.fn();
  close = vi.fn(() => {
    this.onclose?.();
    this.dispatchEvent(new Event("close"));
  });
  constructor(public url: string) {
    super();
    MockWebSocket.instances.push(this);
  }
  deliverMessage(data: unknown) {
    this.onmessage?.({ data });
  }
  fireError() {
    this.onerror?.();
  }
}

function helloFrame(
  overrides: Partial<{
    server_version: string;
    build_commit: string;
    protocol_version: number;
    mode: "Full" | "LobbyOnly";
    lobby_protocol_version: number;
    wire_formats: string[];
  }> = {},
): string {
  return JSON.stringify({
    type: "ServerHello",
    data: {
      server_version: "0.0.0-test",
      build_commit: "testhash",
      protocol_version: PROTOCOL_VERSION,
      mode: "Full",
      ...overrides,
    },
  });
}

beforeEach(() => {
  MockWebSocket.instances = [];
  vi.stubGlobal("WebSocket", MockWebSocket);
});

describe("openPhaseSocket", () => {
  it("uses the browser WebSocket constructor when no factory is supplied", async () => {
    const promise = openPhaseSocket("ws://default-transport");
    const ws = MockWebSocket.instances[0];
    expect(ws.url).toBe("ws://default-transport");
    ws.deliverMessage(helloFrame());

    await expect(promise).resolves.toMatchObject({ ws });
  });

  it("resolves with serverInfo once ServerHello arrives and sends ClientHello", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame());

    const socket = await promise;
    expect(socket.serverInfo.mode).toBe("Full");
    expect(socket.serverInfo.protocolVersion).toBe(PROTOCOL_VERSION);
    expect(ws.send).toHaveBeenCalledWith(
      expect.stringContaining('"type":"ClientHello"'),
    );
  });

  it("negotiates the gzip envelope and queues binary sends in order", async () => {
    const promise = openPhaseSocket("ws://test");
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(helloFrame({ wire_formats: ["GzipEnvelopeV1"] }));

    const socket = await promise;
    expect(socket.serverInfo.wireFormats).toEqual(["GzipEnvelopeV1"]);
    expect(raw.send).toHaveBeenCalledWith(
      expect.stringContaining('"wire_formats":["GzipEnvelopeV1"]'),
    );

    socket.ws.send(JSON.stringify({ type: "Ping", data: { timestamp: 7 } }));
    await vi.waitFor(() => {
      expect(raw.send).toHaveBeenCalledWith(expect.any(Uint8Array));
    });
    const binary = raw.send.mock.calls.find(([value]) => value instanceof Uint8Array)?.[0];
    expect(binary?.[0]).toBe(0x00);

    const order: string[] = [];
    const received = vi.fn((_event: MessageEvent<string>) => order.push("message"));
    socket.ws.onmessage = received;
    socket.ws.addEventListener("close", () => order.push("close"));
    const response = new TextEncoder().encode(JSON.stringify({ type: "Pong" }));
    raw.deliverMessage(new Uint8Array([0x00, ...response]).buffer);
    raw.close();
    await vi.waitFor(() => expect(received).toHaveBeenCalledOnce());
    expect(received).toHaveBeenCalledWith(expect.objectContaining({ data: '{"type":"Pong"}' }));
    expect(order).toEqual(["message", "close"]);
  });

  it("reports a queued binary send failure before closing", async () => {
    const promise = openPhaseSocket("ws://test");
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(helloFrame({ wire_formats: ["GzipEnvelopeV1"] }));

    const socket = await promise;
    const onerror = vi.fn();
    socket.ws.onerror = onerror;
    raw.send.mockImplementationOnce(() => {
      throw new Error("socket closed before queued send");
    });
    socket.ws.send('{"type":"Ping"}');

    await vi.waitFor(() => expect(onerror).toHaveBeenCalledOnce());
    expect(raw.close).toHaveBeenCalled();
  });

  it("rejects with protocol_mismatch when versions diverge and closes the socket", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 99 }));

    await expect(promise).rejects.toBeInstanceOf(HandshakeError);
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects the immediately previous Full protocol before it can omit storm_count", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: PROTOCOL_VERSION - 1 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("accepts the previous protocol version for LobbyOnly brokers", async () => {
    expect(LOBBY_MIN_SUPPORTED_SERVER_PROTOCOL).toBe(PROTOCOL_VERSION - 1);
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(
      helloFrame({ protocol_version: PROTOCOL_VERSION - 1, mode: "LobbyOnly" }),
    );

    const socket = await promise;
    expect(socket.serverInfo.mode).toBe("LobbyOnly");
    expect(socket.serverInfo.protocolVersion).toBe(PROTOCOL_VERSION - 1);
    expect(ws.send).toHaveBeenCalledWith(
      expect.stringContaining(`"protocol_version":${PROTOCOL_VERSION - 1}`),
    );
  });

  // LEGACY PATH: this broker advertises no `lobby_protocol_version`, so the
  // client falls back to the derived `protocol_version` window. Preserved
  // verbatim so already-deployed brokers stay reachable.
  it("rejects LobbyOnly brokers older than the derived one-version window", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(
      helloFrame({ protocol_version: PROTOCOL_VERSION - 2, mode: "LobbyOnly" }),
    );

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });


  // ── Lobby-owned protocol version ────────────────────────────────────────

  it("accepts a LobbyOnly broker with a stale full-game protocol when its lobby version is current", async () => {
    // The regression this whole change exists for. `main` drifting two
    // GameState-only bumps ahead of the deployed broker used to reject here
    // with "Server protocol version N is older than supported".
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(
      helloFrame({
        protocol_version: PROTOCOL_VERSION - 9,
        mode: "LobbyOnly",
        lobby_protocol_version: LOBBY_PROTOCOL_VERSION,
      }),
    );

    const socket = await promise;
    expect(socket.serverInfo.lobbyProtocolVersion).toBe(LOBBY_PROTOCOL_VERSION);
    expect(ws.close).not.toHaveBeenCalled();
  });

  it("accepts a LobbyOnly broker whose lobby version is NEWER than this client", async () => {
    // No ceiling on the lobby surface. This is the case that used to strand
    // every older desktop build at each protocol-bumping release: the broker
    // redeploys, the shipped client cannot, and an upper bound evicts it.
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(
      helloFrame({
        protocol_version: PROTOCOL_VERSION,
        mode: "LobbyOnly",
        lobby_protocol_version: LOBBY_PROTOCOL_VERSION + 5,
      }),
    );

    await expect(promise).resolves.toBeDefined();
    expect(ws.close).not.toHaveBeenCalled();
  });

  it("still refuses a lobby broker below the lobby floor", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(
      helloFrame({
        mode: "LobbyOnly",
        // Measured against the FLOOR, not against this client's own version:
        // an additive bump moves LOBBY_PROTOCOL_VERSION without moving
        // MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL, so `version - 1` is not
        // necessarily below the floor at all.
        lobby_protocol_version: MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL - 1,
      }),
    );

    await expect(promise).rejects.toMatchObject({ kind: "protocol_mismatch" });
    expect(ws.close).toHaveBeenCalled();
  });

  it("holds the full-game surface to an exact match even when the server advertises a lobby version", async () => {
    // A Full server runs the engine; GameState payloads are not compatible
    // across a bump regardless of what it says about the lobby surface.
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(
      helloFrame({
        protocol_version: PROTOCOL_VERSION - 1,
        mode: "Full",
        lobby_protocol_version: LOBBY_PROTOCOL_VERSION,
      }),
    );

    await expect(promise).rejects.toMatchObject({ kind: "protocol_mismatch" });
  });

  // The self-hosted case: a `Full` server pinned to a released image sits behind
  // a client built from main. Its lobby surface is current, so browsing must
  // work; only PLAYING on it is refused, by the exact-match test above.
  it("reaches a Full server's lobby surface when its full-game protocol is stale", async () => {
    const staleHello = helloFrame({
      protocol_version: PROTOCOL_VERSION - 2,
      mode: "Full",
      lobby_protocol_version: LOBBY_PROTOCOL_VERSION,
    });

    // Same server, same frame, default surface: still refused. Without this the
    // assertion below could pass on a server this client would have accepted
    // anyway, proving nothing about the surface parameter.
    const fullSurface = openPhaseSocket("ws://test");
    MockWebSocket.instances[0].deliverMessage(staleHello);
    await expect(fullSurface).rejects.toMatchObject({ kind: "protocol_mismatch" });

    const lobbySurface = openPhaseSocket("ws://test", { surface: "lobby" });
    const ws = MockWebSocket.instances[1];
    ws.deliverMessage(staleHello);
    const socket = await lobbySurface;

    expect(socket.serverInfo.protocolVersion).toBe(PROTOCOL_VERSION - 2);
    // The echo is what an already-deployed server gates on: it has no surface
    // field to branch on, so a hello carrying this client's own newer number
    // would be rejected outright.
    expect(ws.send).toHaveBeenCalledWith(
      expect.stringContaining(`"protocol_version":${PROTOCOL_VERSION - 2}`),
    );
    expect(ws.send).not.toHaveBeenCalledWith(
      expect.stringContaining(`"protocol_version":${PROTOCOL_VERSION}`),
    );
    // ...while still declaring the surface version a lobby-aware server gates on.
    expect(ws.send).toHaveBeenCalledWith(
      expect.stringContaining(`"lobby_protocol_version":${LOBBY_PROTOCOL_VERSION}`),
    );
  });

  it("still refuses a lobby-surface socket below the lobby floor", async () => {
    // The surface parameter widens the window it is measured against; it does
    // not waive the measurement.
    const promise = openPhaseSocket("ws://test", { surface: "lobby" });
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(
      helloFrame({
        protocol_version: PROTOCOL_VERSION,
        mode: "Full",
        // See above: below the floor, which is not the same as one below the
        // client's own lobby version.
        lobby_protocol_version: MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL - 1,
      }),
    );

    await expect(promise).rejects.toMatchObject({ kind: "protocol_mismatch" });
  });

  // LEGACY PATH on the lobby surface: a server advertising no lobby version
  // says nothing about its lobby frames, so the derived one-version window on
  // `protocol_version` is all there is to go on.
  it("falls back to the derived window on the lobby surface when no lobby version is advertised", async () => {
    const withinWindow = openPhaseSocket("ws://test", { surface: "lobby" });
    MockWebSocket.instances[0].deliverMessage(
      helloFrame({
        protocol_version: LOBBY_MIN_SUPPORTED_SERVER_PROTOCOL,
        mode: "Full",
      }),
    );
    await expect(withinWindow).resolves.toBeDefined();

    const belowWindow = openPhaseSocket("ws://test", { surface: "lobby" });
    MockWebSocket.instances[1].deliverMessage(
      helloFrame({
        protocol_version: LOBBY_MIN_SUPPORTED_SERVER_PROTOCOL - 1,
        mode: "Full",
      }),
    );
    await expect(belowWindow).rejects.toMatchObject({ kind: "protocol_mismatch" });
  });

  it("always declares its own lobby protocol version in ClientHello", async () => {
    // Sent unconditionally: brokers that predate the field ignore it, and a
    // broker that understands it gates on this instead of protocol_version.
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ mode: "LobbyOnly" }));
    await promise;

    expect(ws.send).toHaveBeenCalledWith(
      expect.stringContaining(`"lobby_protocol_version":${LOBBY_PROTOCOL_VERSION}`),
    );
  });

  it("times out and closes the socket when ServerHello never arrives", async () => {
    vi.useFakeTimers();
    try {
      // Attach the `.catch` before advancing timers so the rejection
      // lands on a consumer rather than bubbling to `unhandledrejection`
      // when the timer fires synchronously under fake-timer advance.
      const errPromise = openPhaseSocket("ws://test", { timeoutMs: 100 }).catch(
        (e) => e as HandshakeError,
      );
      const ws = MockWebSocket.instances[0];
      await vi.advanceTimersByTimeAsync(200);
      const err = await errPromise;
      expect(err).toBeInstanceOf(HandshakeError);
      expect((err as HandshakeError).kind).toBe("timeout");
      expect(ws.close).toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("closes the in-flight socket synchronously when signal aborts", async () => {
    const ac = new AbortController();
    const promise = openPhaseSocket("ws://test", { signal: ac.signal });
    const ws = MockWebSocket.instances[0];
    ac.abort();
    const err = await promise.catch((e) => e);
    expect(err).toBeInstanceOf(HandshakeError);
    expect((err as HandshakeError).kind).toBe("aborted");
    // Critical: the socket must be closed before the promise rejects so
    // callers don't observe a half-open connection.
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects immediately if the signal is already aborted", async () => {
    const ac = new AbortController();
    ac.abort();
    await expect(
      openPhaseSocket("ws://test", { signal: ac.signal }),
    ).rejects.toBeInstanceOf(HandshakeError);
  });
});

describe("withReconnect", () => {
  it("invokes the factory once on start and exposes the current socket", async () => {
    const factory = vi.fn(async () => {
      const ws = new MockWebSocket("ws://test") as unknown as WebSocket;
      return {
        ws,
        serverInfo: {
          version: "",
          buildCommit: "",
          protocolVersion: 1,
          mode: "Full" as const,
        },
        close: () => (ws as unknown as MockWebSocket).close(),
      };
    });

    const states: string[] = [];
    const handle = withReconnect(factory, {
      onStateChange: (s) => states.push(s),
    });

    await new Promise((r) => setTimeout(r, 0));
    expect(factory).toHaveBeenCalledTimes(1);
    expect(handle.current()).not.toBeNull();
    expect(states).toContain("open");
    handle.close();
  });

  it("retries up to the configured number of attempts then transitions to offline", async () => {
    vi.useFakeTimers();
    try {
      const factory = vi.fn(async () => {
        throw new HandshakeError("ws_error", "simulated");
      });

      const states: string[] = [];
      const handle = withReconnect(factory, {
        attempts: 2,
        backoffMs: () => 10,
        onStateChange: (s) => states.push(s),
      });

      // Initial attempt fails → reconnecting → retry1 fails → reconnecting
      //   → retry2 fails → offline.
      for (let i = 0; i < 5; i++) {
        await vi.advanceTimersByTimeAsync(20);
      }

      expect(factory.mock.calls.length).toBeGreaterThanOrEqual(3);
      expect(states).toContain("offline");
      handle.close();
    } finally {
      vi.useRealTimers();
    }
  });
});
