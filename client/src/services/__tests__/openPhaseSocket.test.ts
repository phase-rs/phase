import { beforeEach, describe, expect, it, vi } from "vitest";

const { lanSupported, probeLan, authorizeLan, invokeLan, channelListener } = vi.hoisted(() => ({
  lanSupported: vi.fn(), probeLan: vi.fn(), authorizeLan: vi.fn(), invokeLan: vi.fn(),
  channelListener: { current: null as null | ((event: unknown) => void) },
}));
vi.mock("../lan", async (importOriginal) => ({
  ...await importOriginal<typeof import("../lan")>(),
  canUseLanBridge: lanSupported, initializeLanCapabilities: probeLan, authorizeLanServer: authorizeLan,
}));
vi.mock("../platform", () => ({ isDesktopTauri: () => true }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeLan,
  Channel: class { constructor(callback: (event: unknown) => void) { channelListener.current = callback; } },
}));

import {
  HandshakeError,
  openPhaseSocket,
  withReconnect,
  writeSetupFrame,
  type PhaseSocketTransport,
} from "../openPhaseSocket";
import {
  LOBBY_MIN_SUPPORTED_SERVER_PROTOCOL,
  LOBBY_PROTOCOL_VERSION,
  MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL,
  PROTOCOL_VERSION,
} from "../../adapter/ws-adapter";
import { decodeJsonEnvelope, encodeJsonEnvelope, WIRE_COMPRESSION_THRESHOLD } from "../../network/wireEnvelope";

class MockWebSocket extends EventTarget {
  static OPEN = 1;
  static CLOSED = 3;
  static instances: MockWebSocket[] = [];
  readyState = MockWebSocket.OPEN;
  binaryType: BinaryType = "blob";
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;
  send = vi.fn();
  close = vi.fn(() => this.fireClose());
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
  fireClose(event = new CloseEvent("close")) {
    this.readyState = MockWebSocket.CLOSED;
    this.onclose?.(event);
    this.dispatchEvent(event);
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
    command_correlation: unknown;
    state_codecs: unknown;
    selected_state_codec: unknown;
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
  lanSupported.mockReturnValue(false); probeLan.mockResolvedValue(false);
  authorizeLan.mockReset().mockResolvedValue(undefined);
  invokeLan.mockResolvedValue(41); channelListener.current = null;
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

  it.each([
    ["CommandIdV1", "CommandIdV1"],
    ["Other", undefined],
    [1, undefined],
    [undefined, undefined],
  ])("parses command_correlation %j as %j", async (advertised, expected) => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ command_correlation: advertised }));

    const socket = await promise;
    expect(socket.serverInfo.commandCorrelation).toBe(expected);
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
    const received = vi.fn((_event: MessageEvent<string>) => { order.push("message"); });
    socket.ws.onmessage = received;
    socket.ws.addEventListener("close", () => order.push("close"));
    const response = new TextEncoder().encode(JSON.stringify({ type: "Pong" }));
    raw.deliverMessage(new Uint8Array([0x00, ...response]).buffer);
    raw.close();
    await vi.waitFor(() => expect(received).toHaveBeenCalledOnce());
    expect(received).toHaveBeenCalledWith(expect.objectContaining({ data: '{"type":"Pong"}' }));
    expect(order).toEqual(["message", "close"]);
  });

  it("bounds retained receive slots during sustained binary traffic while preserving FIFO", async () => {
    const pending = openPhaseSocket("ws://test");
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(helloFrame({ wire_formats: ["GzipEnvelopeV1"] }));
    const socket = await pending;
    const total = 1024;
    const maximumPendingTail = 2;
    let peakRetainedSlots = 0;
    const sampleRetainedSlots = () => {
      // Read-only resource evidence: cleared-but-retained prefix slots still
      // consume storage, even when their payload references are undefined.
      const storage: unknown = Reflect.get(socket.ws, "pendingReceives");
      peakRetainedSlots = Math.max(
        peakRetainedSlots,
        Array.isArray(storage) ? storage.length : Number.POSITIVE_INFINITY,
      );
    };
    const received: number[] = [];
    let next = 2;
    let finish!: () => void;
    const completed = new Promise<void>((resolve) => { finish = resolve; });
    const frame = (timestamp: number) => {
      const bytes = new TextEncoder().encode(JSON.stringify({ type: "Pong", data: { timestamp } }));
      return new Uint8Array([0x00, ...bytes]).buffer;
    };
    socket.ws.onmessage = (event) => {
      const message: { data: { timestamp: number } } = JSON.parse(event.data);
      received.push(message.data.timestamp);
      sampleRetainedSlots();
      // There is already one successor queued. Admit another before returning,
      // so promotion always leaves a nonempty tail throughout the stream.
      if (next < total) raw.deliverMessage(frame(next++));
      sampleRetainedSlots();
      if (received.length === total) finish();
    };
    try {
      raw.deliverMessage(frame(0));
      raw.deliverMessage(frame(1));
      await completed;
      expect(received).toEqual(Array.from({ length: total }, (_, index) => index));
      expect(peakRetainedSlots).toBeLessThanOrEqual(2 * maximumPendingTail);
    } finally {
      socket.close();
    }
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

  it("reports a receive decode failure before closing", async () => {
    const promise = openPhaseSocket("ws://test");
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(helloFrame({ wire_formats: ["GzipEnvelopeV1"] }));

    const socket = await promise;
    const onerror = vi.fn();
    const received = vi.fn();
    socket.ws.onerror = onerror;
    socket.ws.onmessage = received;

    const pong = new TextEncoder().encode('{"type":"Pong"}');
    raw.deliverMessage(new Uint8Array([0x00, ...pong]).buffer);
    await vi.waitFor(() => expect(received).toHaveBeenCalledOnce());
    expect(onerror).not.toHaveBeenCalled();
    expect(raw.close).not.toHaveBeenCalled();

    const corrupt = await encodeJsonEnvelope(
      JSON.stringify({ type: "Pong", data: "x".repeat(512) }),
    );
    corrupt[0] = 0x02;
    raw.deliverMessage(corrupt.buffer);
    await vi.waitFor(() => expect(onerror).toHaveBeenCalledOnce());
    expect(raw.close).toHaveBeenCalled();
    expect(received).toHaveBeenCalledOnce();
  });

  // Guards the rejected wholesale-catch design: a throwing message listener must
  // not close the socket, because the unwrapped plain-text transport never does.
  it("does not close the socket when a message listener throws", async () => {
    const promise = openPhaseSocket("ws://test");
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(helloFrame({ wire_formats: ["GzipEnvelopeV1"] }));

    const socket = await promise;
    const seen = vi.fn(() => {
      throw new Error("listener blew up");
    });
    socket.ws.addEventListener("message", seen);

    const pong = new TextEncoder().encode('{"type":"Pong"}');
    raw.deliverMessage(new Uint8Array([0x00, ...pong]).buffer);
    raw.deliverMessage(new Uint8Array([0x00, ...pong]).buffer);

    await vi.waitFor(() => expect(seen).toHaveBeenCalledTimes(2));
    expect(raw.close).not.toHaveBeenCalled();
  });

  it("closes after a receive decode failure when the error handler throws", async () => {
    const promise = openPhaseSocket("ws://test");
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(helloFrame({ wire_formats: ["GzipEnvelopeV1"] }));

    const socket = await promise;
    const onerror = vi.fn(() => {
      throw new Error("error handler blew up");
    });
    socket.ws.onerror = onerror;

    const corrupt = await encodeJsonEnvelope(
      JSON.stringify({ type: "Pong", data: "x".repeat(512) }),
    );
    corrupt[0] = 0x02;
    raw.deliverMessage(corrupt.buffer);

    await vi.waitFor(() => expect(onerror).toHaveBeenCalledOnce());
    expect(raw.close).toHaveBeenCalled();
  });

  it("closes after a queued send failure when the error handler throws", async () => {
    const promise = openPhaseSocket("ws://test");
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(helloFrame({ wire_formats: ["GzipEnvelopeV1"] }));

    const socket = await promise;
    const onerror = vi.fn(() => {
      throw new Error("error handler blew up");
    });
    socket.ws.onerror = onerror;
    raw.send.mockImplementationOnce(() => {
      throw new Error("socket closed before queued send");
    });
    socket.ws.send('{"type":"Ping"}');

    await vi.waitFor(() => expect(onerror).toHaveBeenCalledOnce());
    expect(raw.close).toHaveBeenCalled();
  });

  it("notifies close listeners after the close handler throws", async () => {
    const promise = openPhaseSocket("ws://test");
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(helloFrame({ wire_formats: ["GzipEnvelopeV1"] }));

    const socket = await promise;
    const onclose = vi.fn(() => {
      throw new Error("close handler blew up");
    });
    const dropped = vi.fn();
    socket.ws.onclose = onclose;
    socket.ws.addEventListener("close", dropped);

    raw.close();
    await vi.waitFor(() => expect(onclose).toHaveBeenCalledOnce());
    expect(dropped).toHaveBeenCalledOnce();
  });

  it("rejects with protocol_mismatch when versions diverge and closes the socket", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: PROTOCOL_VERSION + 1 }));

    await expect(promise).rejects.toBeInstanceOf(HandshakeError);
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects the immediately previous Full protocol and closes the socket", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: PROTOCOL_VERSION - 1 }));
    await expect(promise).rejects.toMatchObject({ kind: "protocol_mismatch" });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v142 before it can read modal history keyed by incarnation", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 142 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v144 before it can read the SearchLibrary missing-referent tag", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 144 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v143 before it can read one-time boon tags", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 143 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v145 before it can read a counted loop offer", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 145 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v146 before it can read a physical contest", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 146 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v147 before it can read a top-card exile contest", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 147 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v148 before it can read the carrier-owned final-part marker", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 148 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v149 before it can read the gains rider's reshaped payload", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 149 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v150 before it can read a populated Bestow payload", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 150 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });


  it("rejects Full v141 before it can read a linked-exile snapshot's incarnation", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 141 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });
  it("rejects Full v140 before it can read a cast-cost exile link", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 140 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v152 before it can read a deferred counter-cost removal", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 152 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v151 before it can read the shortcut suggestion", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 151 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it.each([152, 153, 154, 155, 156, 157, 158])("rejects Full v%s before it can read a parked ChangeZone frame's pre-event member entries or a merged coin-flip run", async (version) => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: version }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it.each([154, 155, 156])("rejects Full v%s before it can read a TotalManaValue zone choice", async (version) => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: version }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("accepts the current Full protocol", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: PROTOCOL_VERSION }));

    await expect(promise).resolves.toBeDefined();
    expect(ws.close).not.toHaveBeenCalled();
  });

  it("rejects Full v139 before it can read the attack event's attacked players", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 139 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it.each([134, 135, 136, 137, 138])("rejects Full v%s before it can read the crew relation", async (version) => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: version }));

    await expect(promise).rejects.toMatchObject({ kind: "protocol_mismatch" });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v133 before it can read the activation's stack entry", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 133 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v131 before it can read the stack-exile rider's incarnation", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 131 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v130 before it can read the coordinated player scope", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 130 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v129 before it can read the incarnation-bounded linked-exile LKI", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 129 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v128 before it can read the loop-shortcut offer road", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 128 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v127 before it can read a declared-player reference", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 127 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v126 before it can read the AttachedTo attachment referent", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 126 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v122 before it can read the prepared-spell state", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 122 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v121 before it can read the triggering-source grantee", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 121 }));

    await expect(promise).rejects.toMatchObject({
      kind: "protocol_mismatch",
    });
    expect(ws.close).toHaveBeenCalled();
  });

  it("rejects Full v120 before it can read player-relative mana history", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 120 }));

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


describe("LAN default transport", () => {
  it("waits for native approval before opening a socket or starting its timeout", async () => {
    vi.useFakeTimers();
    try {
      lanSupported.mockReturnValue(true); probeLan.mockResolvedValue(true);
      let approve!: () => void;
      authorizeLan.mockImplementation(() => new Promise<void>((resolve) => { approve = resolve; }));
      const pending = openPhaseSocket("ws://192.168.1.2:9374/ws", { timeoutMs: 20 });
      await vi.advanceTimersByTimeAsync(30);
      expect(authorizeLan).toHaveBeenCalledOnce();
      expect(channelListener.current).toBeNull();
      expect(MockWebSocket.instances).toHaveLength(0);
      approve();
      await vi.advanceTimersByTimeAsync(0);
      expect(channelListener.current).not.toBeNull();
      channelListener.current?.({ type: "message", text: helloFrame() });
      (await pending).close();
    } finally {
      vi.useRealTimers();
    }
  });

  it.each(["resolve", "reject"] as const)("aborts a pending capability probe before its late %s", async (completion) => {
    lanSupported.mockReturnValue(true);
    let complete!: () => void;
    const probe = new Promise<boolean>((resolve, reject) => {
      complete = () => completion === "resolve" ? resolve(true) : reject(new Error("Late probe failure"));
    });
    probeLan.mockReturnValue(probe);
    const controller = new AbortController();
    const pending = openPhaseSocket("ws://192.168.1.2:9374/ws", { signal: controller.signal });
    controller.abort();
    await expect(pending).rejects.toMatchObject({ kind: "aborted" });
    complete();
    await probe.catch(() => {});
    expect(authorizeLan).not.toHaveBeenCalled();
    expect(channelListener.current).toBeNull();
    expect(MockWebSocket.instances).toHaveLength(0);
  });

  it.each(["resolve", "reject"] as const)("aborts pending native approval before its late %s", async (completion) => {
    lanSupported.mockReturnValue(true); probeLan.mockResolvedValue(true);
    let complete!: () => void;
    const approval = new Promise<void>((resolve, reject) => {
      complete = () => completion === "resolve" ? resolve() : reject(new Error("Late approval failure"));
    });
    authorizeLan.mockReturnValue(approval);
    const controller = new AbortController();
    const pending = openPhaseSocket("ws://192.168.1.2:9374/ws", { signal: controller.signal });
    await vi.waitFor(() => expect(authorizeLan).toHaveBeenCalledOnce());
    controller.abort();
    await expect(pending).rejects.toMatchObject({ kind: "aborted" });
    complete();
    await approval.catch(() => {});
    expect(channelListener.current).toBeNull();
    expect(MockWebSocket.instances).toHaveLength(0);
  });

  it("never opens a socket when native approval is rejected", async () => {
    lanSupported.mockReturnValue(true); probeLan.mockResolvedValue(true);
    authorizeLan.mockRejectedValue(new Error("LAN approval denied"));
    await expect(openPhaseSocket("ws://192.168.1.2:9374/ws")).rejects.toThrow("LAN approval denied");
    expect(channelListener.current).toBeNull();
    expect(MockWebSocket.instances).toHaveLength(0);
  });

  it("selects native IPC after its capability probe and negotiates text only", async () => {
    lanSupported.mockReturnValue(true); probeLan.mockResolvedValue(true);
    const pending = openPhaseSocket("ws://192.168.1.2:9374/ws");
    await vi.waitFor(() => expect(channelListener.current).not.toBeNull());
    channelListener.current?.({ type: "message", text: helloFrame({ wire_formats: ["GzipEnvelopeV1"] }) });
    const socket = await pending;
    expect(MockWebSocket.instances).toHaveLength(0);
    expect(invokeLan).toHaveBeenCalledWith("connect_lan_server", expect.objectContaining({ url: "ws://192.168.1.2:9374/ws" }));
    expect(invokeLan).toHaveBeenCalledWith("lan_bridge_send", {
      id: 41, text: expect.stringContaining('"wire_formats":[]'),
    });
    socket.close();
  });

  it("keeps the codec off the native bridge: no offer, lease released, text hello unchanged", async () => {
    lanSupported.mockReturnValue(true); probeLan.mockResolvedValue(true);
    const fake = fakeSession();
    const pending = openPhaseSocket("ws://192.168.1.2:9374/ws", { stateCodecSession: fake.session });
    await vi.waitFor(() => expect(channelListener.current).not.toBeNull());
    channelListener.current?.({
      type: "message",
      text: helloFrame({ state_codecs: ["StateCodecV1"], wire_formats: ["GzipEnvelopeV1"] }),
    });
    const socket = await pending;
    const hello = invokeLan.mock.calls.find(([command]) => command === "lan_bridge_send")?.[1] as { text: string };
    expect(JSON.parse(hello.text).data).not.toHaveProperty("state_codecs");
    expect(JSON.parse(hello.text).data.wire_formats).toEqual([]);
    expect(fake.close).toHaveBeenCalledOnce();
    socket.close();
  });

  it("keeps an explicit factory authoritative for a LAN address", async () => {
    lanSupported.mockReturnValue(true);
    const factory = vi.fn((url: string) => new MockWebSocket(url) as unknown as WebSocket);
    const pending = openPhaseSocket("ws://192.168.1.2:9374/ws", { socketFactory: factory });
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame());
    (await pending).close();
    expect(factory).toHaveBeenCalledOnce();
    expect(authorizeLan).not.toHaveBeenCalled();
    expect(channelListener.current).toBeNull();
  });

  it("keeps browser and old-shell LAN connections on WebSocket", async () => {
    const pending = openPhaseSocket("ws://192.168.1.2:9374/ws");
    await vi.waitFor(() => expect(MockWebSocket.instances).toHaveLength(1));
    MockWebSocket.instances[0].deliverMessage(helloFrame());
    (await pending).close();
    expect(authorizeLan).not.toHaveBeenCalled();
    expect(channelListener.current).toBeNull();
  });
});

// ── Negotiated StateCodecV1 delivery ────────────────────────────────────
//
// These rows drive the real handshake helper, the real codec transport and the
// real receive queue over the stubbed transport. The decoder itself is
// substituted: orchestration is what this file proves. Exact reconstruction is
// `stateCodec.integration.test.ts`'s real-WASM job.

import type { Mock } from "vitest";
import type {
  StateCodecDecodeResult,
  StateCodecSession,
} from "../../adapter/state-codec-runtime";
import {
  STATE_CODEC_SOCKET_TIMEOUT_MS,
  StateCodecAttachment,
  readGameStartedFullCredentials,
  type StateCodecFailure,
  type StateCodecRecoveryNotice,
} from "../stateCodecSocket";

const DECODED_PONG = '{"type":"Pong","data":{"timestamp":11}}';

type DecodeHandler = (
  frame: Uint8Array,
) => { kind: "decoded"; json: string } | { kind: "resyncRequired" };

function fakeSession(handler?: DecodeHandler): {
  session: StateCodecSession;
  decode: Mock;
  close: Mock;
} {
  const decode = vi.fn(async (frame: Uint8Array): Promise<StateCodecDecodeResult> =>
    handler ? handler(frame) : { kind: "decoded", json: DECODED_PONG });
  const close = vi.fn();
  return { session: { decode, close }, decode, close };
}

/** The second, plain-text hello that confirms the negotiated codec. */
function confirmationFrame(selected: unknown = "StateCodecV1"): string {
  return JSON.stringify({
    type: "ServerHello",
    data: { server_version: "0.0.0-test", selected_state_codec: selected },
  });
}

function codecFrame(type: "Full" | "Delta"): string {
  return JSON.stringify({ type, data: { state: { type: "Pong" }, revision: 1 } });
}

function fullHello(overrides: Parameters<typeof helloFrame>[0] = {}): string {
  return helloFrame({ state_codecs: ["StateCodecV1"], ...overrides });
}

describe("openPhaseSocket — negotiated StateCodecV1", () => {
  it("advertises, confirms once, and emits the decoded message text", async () => {
    const fake = fakeSession();
    const promise = openPhaseSocket("ws://test", {
      stateCodecSession: fake.session,
      stateCodecBootstrap: () => "accepted",
    });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(fullHello());
    const socket = await promise;

    expect(socket.serverInfo.stateCodecs).toEqual(["StateCodecV1"]);
    expect(raw.send).toHaveBeenCalledWith(
      expect.stringContaining('"state_codecs":["StateCodecV1"]'),
    );

    const received = vi.fn();
    socket.ws.onmessage = received;
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(codecFrame("Full"));

    await vi.waitFor(() => expect(received).toHaveBeenCalledOnce());
    expect(received.mock.calls[0][0].data).toBe(DECODED_PONG);
    expect(fake.decode).toHaveBeenCalledOnce();
    const frame = fake.decode.mock.calls[0][0] as Uint8Array;
    expect(frame).toBeInstanceOf(Uint8Array);
    // The ORIGINAL text crosses the boundary; the routing parse is never
    // re-serialized.
    expect(new TextDecoder().decode(frame)).toBe(codecFrame("Full"));
  });

  it.each([
    ["no prepared session", () => undefined],
    ["an explicit null session", () => null],
  ])("does not advertise with %s", async (_label, build) => {
    const promise = openPhaseSocket("ws://test", {
      stateCodecSession: build() as StateCodecSession | null | undefined,
    });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(fullHello());
    await promise;

    expect(raw.send).not.toHaveBeenCalledWith(
      expect.stringContaining("state_codecs"),
    );
  });

  it.each([
    ["a server that offers no codec", helloFrame()],
    ["a LobbyOnly server", fullHello({ mode: "LobbyOnly" })],
  ])("releases the prepared lease against %s", async (_label, hello) => {
    const fake = fakeSession();
    const promise = openPhaseSocket("ws://test", { stateCodecSession: fake.session });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(hello);
    await promise;

    expect(raw.send).not.toHaveBeenCalledWith(expect.stringContaining("state_codecs"));
    expect(fake.close).toHaveBeenCalledOnce();
    expect(fake.decode).not.toHaveBeenCalled();
  });

  it("keeps the lease on the lobby surface even when the Full protocol matches", async () => {
    const fake = fakeSession();
    const promise = openPhaseSocket("ws://test", {
      surface: "lobby",
      stateCodecSession: fake.session,
    });
    MockWebSocket.instances[0].deliverMessage(fullHello());
    await promise;

    expect(MockWebSocket.instances[0].send).not.toHaveBeenCalledWith(
      expect.stringContaining("state_codecs"),
    );
    expect(fake.close).toHaveBeenCalledOnce();
  });

  it.each([
    ["an unknown name", ["FutureCodec"]],
    ["a mix that keeps only the implemented name", ["FutureCodec", "StateCodecV1"]],
    ["a non-array", { 0: "StateCodecV1" }],
    ["numbers", [1, 2]],
    ["null", null],
  ])("parses state_codecs defensively: %s", async (_label, advertised) => {
    const fake = fakeSession();
    const promise = openPhaseSocket("ws://test", { stateCodecSession: fake.session });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(helloFrame({ state_codecs: advertised }));
    const socket = await promise;

    const advertisedNames = (Array.isArray(advertised) ? advertised : []) as unknown[];
    const expectsCodec = advertisedNames.includes("StateCodecV1");
    if (expectsCodec) {
      expect(socket.serverInfo.stateCodecs).toEqual(["StateCodecV1"]);
      expect(raw.send).toHaveBeenCalledWith(
        expect.stringContaining('"state_codecs":["StateCodecV1"]'),
      );
      return;
    }
    expect(socket.serverInfo.stateCodecs).toBeUndefined();
    expect(raw.send).not.toHaveBeenCalledWith(expect.stringContaining("state_codecs"));
    expect(fake.close).toHaveBeenCalledOnce();
  });

  it.each([
    ["a contradictory selection", () => confirmationFrame("FutureCodec")],
    ["no selection at all", () => JSON.stringify({ type: "ServerHello", data: { server_version: "0.0.0-test" } })],
  ])("closes on %s", async (_label, build) => {
    const fake = fakeSession();
    const promise = openPhaseSocket("ws://test", { stateCodecSession: fake.session });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(fullHello());
    const socket = await promise;
    const closed = vi.fn();
    socket.ws.onclose = closed;
    raw.deliverMessage(build());

    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    await vi.waitFor(() => expect(fake.close).toHaveBeenCalledOnce());
  });

  it.each(["raw", "gzip"] as const)(
    "passes a canonical ordinary %s frame through without a throwaway parse and keeps the noncanonical fallback",
    async (wire) => {
      const fake = fakeSession();
      const { raw, socket } = await openCodecSocket(fake, { gzip: wire === "gzip" });
      const received: string[] = [];
      socket.ws.onmessage = (event) => { received.push(event.data); };
      const canonical = JSON.stringify({
        type: "OpponentDisconnected",
        data: { grace_seconds: 30, full_key: { game_code: "GAME01", generation: 7 } },
      });
      const noncanonical = JSON.stringify({
        data: { grace_seconds: 30, full_key: { game_code: "GAME01", generation: 7 } },
        type: "OpponentDisconnected",
      });
      const deliver = async (text: string) => {
        if (wire === "gzip") raw.deliverMessage((await encodeJsonEnvelope(text)).buffer);
        else raw.deliverMessage(text);
      };
      const parse = vi.spyOn(JSON, "parse");
      try {
        // Reach guard: a canonical frame still reaches the consumer verbatim.
        await deliver(canonical);
        await vi.waitFor(() => expect(received).toEqual([canonical]));
        // The socket already knows the tag from the canonical prefix: the only
        // remaining materialization is the adapter's own dispatch parse.
        expect(parse).not.toHaveBeenCalled();
        parse.mockClear();
        await deliver(noncanonical);
        await vi.waitFor(() => expect(received).toHaveLength(2));
        // Noncanonical field order keeps the compatibility parse.
        expect(parse).toHaveBeenCalledTimes(1);
      } finally {
        parse.mockRestore();
      }
    },
  );

  it("closes when a codec frame arrives before confirmation", async () => {
    const fake = fakeSession();
    const promise = openPhaseSocket("ws://test", { stateCodecSession: fake.session });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(fullHello());
    const socket = await promise;
    const closed = vi.fn();
    socket.ws.onclose = closed;
    raw.deliverMessage(codecFrame("Full"));

    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(fake.decode).not.toHaveBeenCalled();
    expect(fake.close).toHaveBeenCalledOnce();
  });

  // Confirmation has no deadline; the server queues it before application
  // frames. Heartbeat does not bound this wait. A confirmation deadline
  // could fail an initial join after JoinGameWithPassword was sent,
  // orphaning the seat whose player_token only arrives in GameStarted.
  it("tolerates a slow confirmation: no close, frames flow, the lease is held, a late confirmation enables the codec", async () => {
    vi.useFakeTimers();
    try {
      const fake = fakeSession();
      const promise = openPhaseSocket("ws://test", { stateCodecSession: fake.session });
      const raw = MockWebSocket.instances[0];
      raw.deliverMessage(fullHello());
      const socket = await promise;
      const closed = vi.fn();
      const received = vi.fn();
      socket.ws.onclose = closed;
      socket.ws.onmessage = received;

      await vi.advanceTimersByTimeAsync(STATE_CODEC_SOCKET_TIMEOUT_MS * 10);
      expect(closed).not.toHaveBeenCalled();
      expect(raw.close).not.toHaveBeenCalled();
      expect(fake.close).not.toHaveBeenCalled();

      // Reach guard: the transport is alive and still delivering plain frames
      // after the elapsed time, so the assertions above are not vacuous.
      raw.deliverMessage(JSON.stringify({ type: "Pong", data: { timestamp: 3 } }));
      await vi.advanceTimersByTimeAsync(0);
      expect(received).toHaveBeenCalledOnce();
      expect(received.mock.calls[0][0].data).toBe('{"type":"Pong","data":{"timestamp":3}}');
      expect(fake.close).not.toHaveBeenCalled();

      // The late, valid confirmation still enables the codec.
      raw.deliverMessage(confirmationFrame());
      raw.deliverMessage(codecFrame("Full"));
      await vi.advanceTimersByTimeAsync(0);
      expect(fake.decode).toHaveBeenCalledOnce();
      expect(received).toHaveBeenCalledTimes(2);
      expect(received.mock.calls[1][0].data).toBe(DECODED_PONG);
      expect(closed).not.toHaveBeenCalled();

      // The lease is released exactly once, on the eventual close.
      raw.close();
      await vi.advanceTimersByTimeAsync(0);
      expect(closed).toHaveBeenCalledOnce();
      expect(fake.close).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it("releases the lease exactly once on a natural close while still awaiting the confirmation", async () => {
    vi.useFakeTimers();
    try {
      const fake = fakeSession();
      const promise = openPhaseSocket("ws://test", { stateCodecSession: fake.session });
      const raw = MockWebSocket.instances[0];
      raw.deliverMessage(fullHello());
      const socket = await promise;
      const closed = vi.fn();
      socket.ws.onclose = closed;
      socket.ws.onmessage = vi.fn();

      await vi.advanceTimersByTimeAsync(STATE_CODEC_SOCKET_TIMEOUT_MS * 10);
      // Reach guard: nothing closed or released while the confirmation was pending.
      expect(closed).not.toHaveBeenCalled();
      expect(fake.close).not.toHaveBeenCalled();

      raw.close();
      await vi.advanceTimersByTimeAsync(0);
      expect(closed).toHaveBeenCalledOnce();
      expect(fake.close).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it("still fails a codec frame that precedes a slow confirmation", async () => {
    vi.useFakeTimers();
    try {
      const fake = fakeSession();
      const promise = openPhaseSocket("ws://test", { stateCodecSession: fake.session });
      const raw = MockWebSocket.instances[0];
      raw.deliverMessage(fullHello());
      const socket = await promise;
      const closed = vi.fn();
      socket.ws.onclose = closed;
      socket.ws.onmessage = vi.fn();

      await vi.advanceTimersByTimeAsync(STATE_CODEC_SOCKET_TIMEOUT_MS * 10);
      // Reach guard: still open and holding the lease after the elapsed time.
      expect(closed).not.toHaveBeenCalled();
      expect(fake.close).not.toHaveBeenCalled();

      raw.deliverMessage(codecFrame("Delta"));
      await vi.advanceTimersByTimeAsync(0);
      expect(closed).toHaveBeenCalledOnce();
      expect(fake.close).toHaveBeenCalledOnce();
      expect(fake.decode).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("openPhaseSocket — codec stream, recovery and teardown", () => {
  async function negotiated(
    fake: ReturnType<typeof fakeSession>,
    options: {
      bootstrap?: () => "pending" | "accepted";
      onRecovery?: (state: StateCodecRecoveryNotice) => void;
    } = {},
  ) {
    const promise = openPhaseSocket("ws://test", {
      stateCodecSession: fake.session,
      stateCodecBootstrap: options.bootstrap ?? (() => "accepted"),
      onStateCodecRecovery: options.onRecovery,
    });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(fullHello());
    const socket = await promise;
    const received = vi.fn((_event: MessageEvent<string>) => true);
    socket.ws.onmessage = received;
    raw.deliverMessage(confirmationFrame());
    return { raw, socket, received };
  }

  it("passes non-state frames through untouched and in order", async () => {
    const fake = fakeSession();
    const { raw, received } = await negotiated(fake);
    raw.deliverMessage(JSON.stringify({ type: "Pong", data: { timestamp: 5 } }));
    raw.deliverMessage(codecFrame("Full"));

    await vi.waitFor(() => expect(received).toHaveBeenCalledTimes(2));
    expect(received.mock.calls[0][0].data).toBe('{"type":"Pong","data":{"timestamp":5}}');
    expect(received.mock.calls[1][0].data).toBe(DECODED_PONG);
  });

  it("requests one resync, gates deltas, then publishes ready after a recovery Full", async () => {
    const frames: Uint8Array[] = [];
    const fake = fakeSession((frame) => {
      frames.push(frame);
      return frames.length === 1 ? { kind: "resyncRequired" } : { kind: "decoded", json: '{"type":"StateUpdate"}' };
    });
    const recovery: StateCodecRecoveryNotice[] = [];
    const { raw, received } = await negotiated(fake, { onRecovery: (state) => recovery.push(state) });

    raw.deliverMessage(codecFrame("Delta"));
    await vi.waitFor(() => expect(recovery).toEqual(["recovering"]));
    expect(raw.send).toHaveBeenCalledWith('{"type":"ResyncState"}');

    // Deltas and plain notices are suppressed/ordered without extending the
    // window; only a Full clears the latch.
    raw.deliverMessage(codecFrame("Delta"));
    raw.deliverMessage(JSON.stringify({ type: "Error", data: { message: "busy" } }));
    await vi.waitFor(() => expect(received).toHaveBeenCalledOnce());
    expect(received.mock.calls[0][0].data).toBe('{"type":"Error","data":{"message":"busy"}}');

    raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(recovery).toEqual(["recovering", "ready"]));
    expect(received).toHaveBeenCalledTimes(2);
    expect(received.mock.calls[1][0].data).toBe('{"type":"StateUpdate"}');
    expect(
      raw.send.mock.calls.filter(([value]) => value === '{"type":"ResyncState"}'),
    ).toHaveLength(1);
    expect(fake.close).not.toHaveBeenCalled();
  });

  it("requests one resync and closes immediately when bootstrap is pending", async () => {
    // Only the first frame is rejected: every later frame WOULD decode, so a
    // fenced late frame is the wrapper's doing, not the decoder's.
    let calls = 0;
    const fake = fakeSession(() => ++calls === 1
      ? { kind: "resyncRequired" } : { kind: "decoded", json: DECODED_PONG });
    const recovery = vi.fn();
    const { raw, socket, received } = await negotiated(fake, {
      bootstrap: () => "pending", onRecovery: recovery,
    });
    const closed = vi.fn();
    socket.ws.onclose = closed;

    raw.deliverMessage(codecFrame("Delta"));
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(raw.send).toHaveBeenCalledWith('{"type":"ResyncState"}');
    expect(fake.close).toHaveBeenCalledOnce();
    expect(fake.decode).toHaveBeenCalledOnce();
    // A late valid Full and Delta, then a control frame, reach the closed
    // wrapper. The queue is microtask-only, so a macrotask boundary proves it
    // drained them: nothing was decoded, delivered, or published as ready.
    raw.deliverMessage(codecFrame("Full"));
    raw.deliverMessage(codecFrame("Delta"));
    raw.deliverMessage(JSON.stringify({ type: "Pong", data: { timestamp: 3 } }));
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(fake.decode).toHaveBeenCalledOnce();
    expect(received).not.toHaveBeenCalled();
    // A rejection before application bootstrap is the typed terminal reason.
    expect(recovery.mock.calls).toEqual([
      ["recovering"],
      [{ kind: "failed", reason: "bootstrapRejection", joinCredentials: null }],
    ]);
    expect(closed).toHaveBeenCalledOnce();
  });

  it("closes when the recovery Full is rejected as well", async () => {
    const fake = fakeSession(() => ({ kind: "resyncRequired" }));
    const { raw, socket } = await negotiated(fake);
    const closed = vi.fn();
    socket.ws.onclose = closed;

    raw.deliverMessage(codecFrame("Delta"));
    await vi.waitFor(() => expect(raw.send).toHaveBeenCalledWith('{"type":"ResyncState"}'));
    raw.deliverMessage(codecFrame("Full"));

    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(fake.close).toHaveBeenCalledOnce();
    expect(
      raw.send.mock.calls.filter(([value]) => value === '{"type":"ResyncState"}'),
    ).toHaveLength(1);
  });

  it("closes when the recovery window expires and does not extend it", async () => {
    vi.useFakeTimers();
    try {
      const fake = fakeSession(() => ({ kind: "resyncRequired" }));
      const promise = openPhaseSocket("ws://test", {
        stateCodecSession: fake.session,
        stateCodecBootstrap: () => "accepted",
      });
      const raw = MockWebSocket.instances[0];
      raw.deliverMessage(fullHello());
      const socket = await promise;
      const closed = vi.fn();
      socket.ws.onclose = closed;
      socket.ws.onmessage = vi.fn();
      raw.deliverMessage(confirmationFrame());
      raw.deliverMessage(codecFrame("Delta"));
      await vi.advanceTimersByTimeAsync(0);
      expect(raw.send).toHaveBeenCalledWith('{"type":"ResyncState"}');
      await vi.advanceTimersByTimeAsync(STATE_CODEC_SOCKET_TIMEOUT_MS - 1);
      raw.deliverMessage(JSON.stringify({ type: "Pong", data: { timestamp: 1 } }));
      await vi.advanceTimersByTimeAsync(2);
      expect(closed).toHaveBeenCalledOnce();
      expect(fake.close).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it("buffers frames until a handler attaches and delivers each exactly once, close last", async () => {
    const fake = fakeSession();
    const promise = openPhaseSocket("ws://test", {
      stateCodecSession: fake.session,
      stateCodecBootstrap: () => "accepted",
    });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(fullHello());
    const socket = await promise;
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(codecFrame("Full"));
    raw.deliverMessage(JSON.stringify({ type: "Pong", data: { timestamp: 2 } }));
    raw.close();

    const order: string[] = [];
    socket.ws.onmessage = (event) => { order.push(`message:${event.data}`); };
    socket.ws.addEventListener("close", () => order.push("close"));

    await vi.waitFor(() => expect(order).toContain("close"));
    expect(order.filter((entry) => entry === "close")).toHaveLength(1);
    expect(order).toEqual([`message:${DECODED_PONG}`, 'message:{"type":"Pong","data":{"timestamp":2}}', "close"]);
    expect(fake.close).toHaveBeenCalledOnce();
  });

  it("local close releases once and fences a late decode result", async () => {
    let release!: (value: { kind: "decoded"; json: string }) => void;
    const fake = fakeSession();
    fake.decode.mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    const { raw, socket, received } = await negotiated(fake);
    // Reach guard: this handler observes delivered frames before the close.
    raw.deliverMessage(JSON.stringify({ type: "Pong", data: { timestamp: 1 } }));
    await vi.waitFor(() => expect(received).toHaveBeenCalledOnce());
    received.mockClear();
    raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(fake.decode).toHaveBeenCalledOnce());

    socket.close();
    expect(fake.close).toHaveBeenCalledOnce();
    release({ kind: "decoded", json: DECODED_PONG });
    // The queue is microtask-only: a macrotask boundary means the released
    // decode has been fully processed by the wrapper.
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(received).not.toHaveBeenCalled();
    socket.close();
    expect(fake.close).toHaveBeenCalledOnce();
  });

  // A decoder/RPC failure is a transport failure, not codec integrity: the
  // socket must terminate (one close, one lease release) and must not ask the
  // server for a resync it could never decode.
  it.each([
    "State codec decode timed out after 60000ms",
    "State codec session is closed",
  ])("terminates the socket once when the decoder rejects (%s)", async (failure) => {
    const fake = fakeSession();
    fake.decode.mockImplementationOnce(async () => { throw new Error(failure); });
    const { raw, socket, received } = await negotiated(fake);
    const closed = vi.fn();
    socket.ws.onclose = closed;
    // Reach guard: a frame delivered before the failure reaches the handler,
    // so a later "nothing delivered" assertion is not a dead receive path.
    raw.deliverMessage(JSON.stringify({ type: "Pong", data: { timestamp: 1 } }));
    await vi.waitFor(() => expect(received).toHaveBeenCalledOnce());

    raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(fake.decode).toHaveBeenCalledOnce();
    expect(fake.close).toHaveBeenCalledOnce();
    expect(raw.close).toHaveBeenCalledOnce();
    expect(raw.send).not.toHaveBeenCalledWith('{"type":"ResyncState"}');

    // Later frames reach the terminated wrapper (the default decoder WOULD
    // accept them); the queue is microtask-only, so a macrotask boundary proves
    // they were drained without being decoded or delivered.
    raw.deliverMessage(codecFrame("Full"));
    raw.deliverMessage(codecFrame("Delta"));
    raw.deliverMessage(JSON.stringify({ type: "Pong", data: { timestamp: 2 } }));
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(fake.decode).toHaveBeenCalledOnce();
    expect(received).toHaveBeenCalledOnce();
    expect(closed).toHaveBeenCalledOnce();
    expect(fake.close).toHaveBeenCalledOnce();
    expect(raw.close).toHaveBeenCalledOnce();
    expect(raw.send).not.toHaveBeenCalledWith('{"type":"ResyncState"}');
  });

  it.each(["declines", "throws"] as const)(
    "closes when the primary application %s recovery even if an observer acknowledges",
    async (outcome) => {
      let calls = 0;
      const fake = fakeSession(() => ++calls === 1
        ? { kind: "resyncRequired" } : { kind: "decoded", json: DECODED_PONG });
      const recovery: StateCodecRecoveryNotice[] = [];
      const { raw, socket } = await negotiated(fake, { onRecovery: (state) => recovery.push(state) });
      const closed = vi.fn();
      socket.ws.onclose = closed;
      raw.deliverMessage(codecFrame("Delta"));
      await vi.waitFor(() => expect(recovery).toEqual(["recovering"]));
      const primary = vi.fn((_event: MessageEvent<string>) => {
        if (outcome === "throws") throw new Error("application declined before committing");
        return false;
      });
      const observer = vi.fn((_event: MessageEvent<string>) => true);
      socket.ws.onmessage = primary;
      socket.ws.addEventListener("message", observer);
      raw.deliverMessage(codecFrame("Full"));
      await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
      expect(primary).toHaveBeenCalledOnce();
      expect(observer).toHaveBeenCalledOnce();
      expect(fake.decode).toHaveBeenCalledTimes(2);
      expect(recovery).toEqual(["recovering"]);
      expect(fake.close).toHaveBeenCalledOnce();
      expect(raw.close).toHaveBeenCalledOnce();
    },
  );

  it("propagates a listener throw without skipping queue cleanup", async () => {
    const fake = fakeSession();
    const { raw, socket } = await negotiated(fake);
    const closed = vi.fn();
    socket.ws.onmessage = () => { throw new Error("listener blew up"); };
    socket.ws.onclose = closed;
    raw.deliverMessage(codecFrame("Full"));
    raw.close();
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(fake.close).toHaveBeenCalledOnce();
  });
});

describe("openPhaseSocket — bounded expanded codec backlog", () => {
  // Count includes the logical active decode; bytes bound only queued backlog.
  const messageLimit = 64;
  const textByteLimit = 16 * 1024 * 1024;
  const frameCharacters = 1024 * 1024;

  function paddedFrame(type: "Full" | "Delta", characters = 512): string {
    const frame = { type, data: { state: { type: "Pong", padding: "" }, revision: 1 } };
    frame.data.state.padding = "x".repeat(characters - JSON.stringify(frame).length);
    return JSON.stringify(frame);
  }

  async function negotiated(fake: { session: StateCodecSession }, recovery = vi.fn()) {
    const pending = openPhaseSocket("ws://test", {
      stateCodecSession: fake.session,
      stateCodecBootstrap: () => "accepted",
      onStateCodecRecovery: recovery,
    });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(fullHello({ wire_formats: ["GzipEnvelopeV1"] }));
    const socket = await pending;
    const received = vi.fn();
    socket.ws.onmessage = received;
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage('{"type":"Pong"}');
    await vi.waitFor(() => expect(received).toHaveBeenCalledOnce());
    received.mockClear();
    return { raw, socket, received, recovery };
  }

  it.each(["message count", "UTF-16 bytes"] as const)(
    "terminates an expanded %s overload before the active decode settles",
    async (bound) => {
      let release!: (result: StateCodecDecodeResult) => void;
      const fake = fakeSession();
      fake.decode.mockImplementationOnce(() => new Promise<StateCodecDecodeResult>((resolve) => {
        release = resolve;
      }));
      const order: string[] = [];
      const recovery = vi.fn(() => { order.push("failed"); });
      const { raw, socket, received } = await negotiated(fake, recovery);
      const closed = vi.fn(() => { order.push("close"); });
      socket.ws.onclose = closed;
      const characters = bound === "message count" ? 512 : frameCharacters;
      const full = await encodeJsonEnvelope(paddedFrame("Full", characters));
      const delta = await encodeJsonEnvelope(paddedFrame("Delta", characters));
      expect(full[0]).toBe(0x01);
      expect(delta[0]).toBe(0x01);
      raw.deliverMessage(full.buffer);
      await vi.waitFor(() => expect(fake.decode).toHaveBeenCalledOnce());

      if (bound === "message count") {
        // One active Full plus 64 queued Deltas exceeds the 64-message budget.
        for (let index = 0; index < messageLimit; index++) raw.deliverMessage(delta.buffer);
      } else {
        // ASCII occupies two retained UTF-16 bytes per character. Eight queued
        // 1 MiB strings exactly fill 16 MiB, independently of the active Full;
        // a tiny additional frame must fail well below the message-count bound.
        const framesAtLimit = textByteLimit / (characters * 2);
        for (let index = 0; index < framesAtLimit; index++) raw.deliverMessage(delta.buffer);
        raw.deliverMessage((await encodeJsonEnvelope(paddedFrame("Delta"))).buffer);
      }

      await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
      expect(recovery.mock.calls).toEqual([[
        { kind: "failed", reason: "receiveOverload", joinCredentials: null },
      ]]);
      expect(order).toEqual(["failed", "close"]);
      expect(fake.close).toHaveBeenCalledOnce();
      expect(raw.close).toHaveBeenCalledOnce();
      expect(fake.decode).toHaveBeenCalledOnce();
      expect(received).not.toHaveBeenCalled();

      vi.useFakeTimers();
      try {
        release({ kind: "decoded", json: DECODED_PONG });
        raw.deliverMessage(codecFrame("Delta"));
        await vi.advanceTimersByTimeAsync(0);
      } finally {
        vi.useRealTimers();
      }
      expect(received).not.toHaveBeenCalled();
      expect(fake.decode).toHaveBeenCalledOnce();
      expect(closed).toHaveBeenCalledOnce();
      socket.close();
      expect(fake.close).toHaveBeenCalledOnce();
      expect(raw.close).toHaveBeenCalledOnce();
    },
  );

  it("preserves FIFO delivery and restores the message budget as each backlog drains", async () => {
    const fake = fakeSession((frame) => ({
      kind: "decoded",
      json: new TextDecoder().decode(frame),
    }));
    const { raw, socket, received, recovery } = await negotiated(fake);
    const expected: string[] = [];
    for (let wave = 0; wave < 2; wave++) {
      let release!: (result: StateCodecDecodeResult) => void;
      fake.decode.mockImplementationOnce(() => new Promise<StateCodecDecodeResult>((resolve) => {
        release = resolve;
      }));
      const first = paddedFrame(wave === 0 ? "Full" : "Delta");
      raw.deliverMessage((await encodeJsonEnvelope(first)).buffer);
      await vi.waitFor(() => expect(fake.decode).toHaveBeenCalledTimes(wave * messageLimit + 1));
      expected.push(first);
      for (let index = 1; index < messageLimit; index++) {
        const text = JSON.stringify({
          type: "Delta", data: { revision: wave * messageLimit + index, padding: "x".repeat(512) },
        });
        expected.push(text);
        raw.deliverMessage((await encodeJsonEnvelope(text)).buffer);
      }
      release({ kind: "decoded", json: first });
      await vi.waitFor(() => expect(received).toHaveBeenCalledTimes((wave + 1) * messageLimit));
      expect(received.mock.calls.map(([event]) => event.data)).toEqual(expected);
      expect(raw.close).not.toHaveBeenCalled();
    }
    expect(recovery).not.toHaveBeenCalled();
    socket.close();
    expect(fake.close).toHaveBeenCalledOnce();
  });

  // These are scripted-decoder queue tests, not native serializer evidence.
  // The separate largeSpectatorFull integration probe covers genuine native
  // GameStarted/Full bytes with the real WASM decoder and spectator adapter.
  function largeBootstrapFrame(): string {
    const objects = Object.fromEntries(Array.from({ length: 9_000 }, (_, index) => [
      String(index + 1),
      {
        id: index + 1,
        name: "Queue fixture permanent",
        abilities: Array.from({ length: 16 }, () => ({
          type: "Activated", cost: { type: "Tap" }, effect: { type: "DrawCards", count: 1 },
        })),
      },
    ]));
    return JSON.stringify({
      type: "Full",
      data: {
        revision: 100,
        state: {
          type: "GameStarted",
          data: { your_player: 255, state_revision: 100, state: { objects, turn_number: 7 } },
        },
      },
    });
  }

  function bootstrapDecoder(frame: Uint8Array): StateCodecDecodeResult {
    const parsed = JSON.parse(new TextDecoder().decode(frame));
    return { kind: "decoded", json: JSON.stringify(parsed.data.state) };
  }

  it("delivers a standalone Full larger than the stalled backlog and restores capacity", async () => {
    const text = largeBootstrapFrame();
    expect(text.length).toBeGreaterThan(8_388_608);
    const compressed = await encodeJsonEnvelope(text);
    expect(compressed[0]).toBe(0x01);
    const fake = fakeSession(bootstrapDecoder);
    const { raw, socket, received, recovery } = await negotiated(fake);
    const consumer: { revision?: number; turn?: number; objectCount?: number; spectator?: number } = {};
    socket.ws.onmessage = (event) => {
      const message = JSON.parse(event.data);
      consumer.revision = message.data.state_revision;
      consumer.turn = message.data.state.turn_number;
      consumer.objectCount = Object.keys(message.data.state.objects).length;
      consumer.spectator = message.data.your_player;
      received(event);
      return true;
    };
    for (let wave = 0; wave < 2; wave++) {
      raw.deliverMessage(compressed.buffer);
      await vi.waitFor(() => expect(received).toHaveBeenCalledTimes(wave + 1));
      expect(consumer).toEqual({ revision: 100, turn: 7, objectCount: 9_000, spectator: 255 });
      expect(fake.close).not.toHaveBeenCalled();
      expect(raw.close).not.toHaveBeenCalled();
    }
    expect(fake.decode).toHaveBeenCalledTimes(2);
    expect(recovery).not.toHaveBeenCalled();
    // A completed large frame must leave the entire ordinary budget available.
    let release!: (result: StateCodecDecodeResult) => void;
    fake.decode.mockImplementationOnce(() => new Promise<StateCodecDecodeResult>((resolve) => {
      release = resolve;
    }));
    const ordinary = await encodeJsonEnvelope(paddedFrame("Delta", frameCharacters));
    raw.deliverMessage(ordinary.buffer);
    await vi.waitFor(() => expect(fake.decode).toHaveBeenCalledTimes(3));
    for (let index = 0; index < textByteLimit / (frameCharacters * 2); index++) {
      raw.deliverMessage(ordinary.buffer);
    }
    socket.ws.onmessage = received;
    fake.decode.mockImplementation(() => ({ kind: "decoded", json: DECODED_PONG }));
    release({ kind: "decoded", json: DECODED_PONG });
    await vi.waitFor(() => expect(received).toHaveBeenCalledTimes(11));
    expect(recovery).not.toHaveBeenCalled();
    socket.close();
    expect(fake.close).toHaveBeenCalledOnce();
    expect(raw.close).toHaveBeenCalledOnce();
  });

  it.each(["ordinary backlog bytes", "another oversized Full"] as const)(
    "bounds %s behind a stalled oversized Full and fences its late result",
    async (bound) => {
      const compressed = await encodeJsonEnvelope(largeBootstrapFrame());
      expect(compressed[0]).toBe(0x01);
      let release!: (result: StateCodecDecodeResult) => void;
      const fake = fakeSession();
      fake.decode.mockImplementationOnce(() => new Promise<StateCodecDecodeResult>((resolve) => {
        release = resolve;
      }));
      const { raw, socket, received, recovery } = await negotiated(fake);
      const closed = vi.fn();
      socket.ws.onclose = closed;
      raw.deliverMessage(compressed.buffer);
      await vi.waitFor(() => expect(fake.decode).toHaveBeenCalledOnce());
      expect(raw.close).not.toHaveBeenCalled();
      expect(fake.close).not.toHaveBeenCalled();
      if (bound === "another oversized Full") {
        raw.deliverMessage(compressed.buffer);
      } else {
        const delta = await encodeJsonEnvelope(paddedFrame("Delta", frameCharacters));
        for (let index = 0; index < textByteLimit / (frameCharacters * 2); index++) {
          raw.deliverMessage(delta.buffer);
        }
        // This must be the overflowing frame, not the active large Full.
        raw.deliverMessage((await encodeJsonEnvelope(paddedFrame("Delta"))).buffer);
      }
      await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
      expect(recovery.mock.calls).toEqual([[
        { kind: "failed", reason: "receiveOverload", joinCredentials: null },
      ]]);
      expect(fake.close).toHaveBeenCalledOnce();
      expect(raw.close).toHaveBeenCalledOnce();
      expect(fake.decode).toHaveBeenCalledOnce();
      expect(received).not.toHaveBeenCalled();
      vi.useFakeTimers();
      try {
        release({ kind: "decoded", json: '{"type":"GameStarted","data":{"state_revision":100}}' });
        await vi.advanceTimersByTimeAsync(0);
      } finally {
        vi.useRealTimers();
      }
      expect(received).not.toHaveBeenCalled();
      expect(fake.decode).toHaveBeenCalledOnce();
      socket.close();
      expect(fake.close).toHaveBeenCalledOnce();
      expect(raw.close).toHaveBeenCalledOnce();
    },
  );
});

describe("openPhaseSocket — canonical codec discriminator", () => {
  // Force real gzip, rather than the negotiated format's small raw envelope.
  const gzipFixtureLength = WIRE_COMPRESSION_THRESHOLD + 1;

  async function openWire(compressed: boolean, fake: { session: StateCodecSession }) {
    const recovery: StateCodecRecoveryNotice[] = [];
    const pending = openPhaseSocket("ws://test", {
      stateCodecSession: fake.session,
      stateCodecBootstrap: () => "accepted",
      onStateCodecRecovery: (state) => { recovery.push(state); },
    });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(fullHello(compressed ? { wire_formats: ["GzipEnvelopeV1"] } : {}));
    const socket = await pending;
    const received: string[] = [];
    socket.ws.onmessage = (event) => { received.push(event.data); return true; };
    raw.deliverMessage(confirmationFrame());
    const deliver = async (text: string) => {
      if (!compressed) {
        raw.deliverMessage(text);
        return;
      }
      const encoded = await encodeJsonEnvelope(text);
      expect(encoded[0]).toBe(0x01);
      raw.deliverMessage(encoded.buffer);
    };
    return { raw, socket, received, recovery, deliver };
  }

  it.each([
    [false, "Full"], [false, "Delta"], [true, "Full"], [true, "Delta"],
  ] as const)("lets the decoder reject a malformed canonical body (gzip=%s, %s)", async (compressed, type) => {
    const fake = fakeSession();
    fake.decode.mockResolvedValueOnce({ kind: "resyncRequired" });
    const wire = await openWire(compressed, fake);
    const text = `{"type":"${type}","data":not-json}`.padEnd(gzipFixtureLength, " ");
    try {
      await wire.deliver(text);
      await vi.waitFor(() => expect(wire.recovery).toEqual(["recovering"]));
      expect(fake.decode).toHaveBeenCalledOnce();
      expect(new TextDecoder().decode(fake.decode.mock.calls[0][0])).toBe(text);
      expect(wire.received).toEqual([]);
      await vi.waitFor(() => expect(wire.raw.send).toHaveBeenCalledTimes(2));
      const sent: unknown = wire.raw.send.mock.calls[1][0];
      const resync = typeof sent === "string" ? sent
        : sent instanceof Uint8Array ? await decodeJsonEnvelope(sent) : null;
      expect(resync).toBe('{"type":"ResyncState"}');

      await wire.deliver(codecFrame("Full").padEnd(gzipFixtureLength, " "));
      await vi.waitFor(() => expect(wire.received).toEqual([DECODED_PONG]));
      expect(wire.recovery).toEqual(["recovering", "ready"]);
      expect(wire.raw.close).not.toHaveBeenCalled();
    } finally {
      wire.socket.close();
    }
  });

  it.each([false, true])("routes large canonical Full and Delta frames in order (gzip=%s)", async (compressed) => {
    const fake = fakeSession();
    const wire = await openWire(compressed, fake);
    const padding = "x".repeat(1024 * 1024 + 1);
    const frames = [
      JSON.stringify({ type: "Full", data: { state: { padding }, revision: 1 } }),
      JSON.stringify({ type: "Delta", data: { patch: { padding }, revision: 2 } }),
    ];
    try {
      for (const text of frames) await wire.deliver(text);
      await vi.waitFor(() => expect(wire.received).toEqual([DECODED_PONG, DECODED_PONG]));
      expect(fake.decode.mock.calls.map(([frame]) => new TextDecoder().decode(frame))).toEqual(frames);
      expect(wire.recovery).toEqual([]);
      expect(wire.raw.close).not.toHaveBeenCalled();
    } finally {
      wire.socket.close();
    }
  });

  it.each([false, true])("preserves ordinary and malformed-header delivery (gzip=%s)", async (compressed) => {
    const fake = fakeSession();
    const wire = await openWire(compressed, fake);
    const plain = [
      '{"type":"Pong","data":{"type":"Full"}}',
      '{"type":"Fullness","data":{}}',
      '{"type":"DeltaSuffix","data":{}}',
      '{"type":"Pong","data":',
      '{"type":',
      '{"type":"Full" "data":{}}',
      '[{"type":"Full","data":{}}]',
    ].map((text) => text.padEnd(gzipFixtureLength, " "));
    try {
      for (const text of plain) await wire.deliver(text);
      await vi.waitFor(() => expect(wire.received).toEqual(plain));
      expect(fake.decode).not.toHaveBeenCalled();
      expect(wire.recovery).toEqual([]);
    } finally {
      wire.socket.close();
    }
  });

  const escapedCodecTags: Array<["Full" | "Delta", boolean]> = [
    ["Full", false], ["Delta", false], ["Full", true], ["Delta", true],
  ];
  it.each(escapedCodecTags)("decodes a compact escaped %s tag (gzip=%s)", async (type, compressed) => {
    const fake = fakeSession();
    const wire = await openWire(compressed, fake);
    // Compact canonical envelope whose tag is escaped: the prefix branch must
    // not hand the raw `Fu\u006cl` slice to ordinary pass-through.
    const tag = type === "Full" ? "Fu\\u006cl" : "Delt\\u0061";
    const text = `{"type":"${tag}","data":{"state":{"type":"Pong"},"revision":1}}`
      .padEnd(gzipFixtureLength, " ");
    try {
      await wire.deliver(text);
      await vi.waitFor(() => expect(wire.received).toEqual([DECODED_PONG]));
      expect(fake.decode).toHaveBeenCalledOnce();
      expect(new TextDecoder().decode(fake.decode.mock.calls[0][0])).toBe(text);
      expect(wire.recovery).toEqual([]);
      expect(wire.raw.close).not.toHaveBeenCalled();
    } finally {
      wire.socket.close();
    }
  });

  it.each([false, true])("retains noncanonical field order and escaped codec tags (gzip=%s)", async (compressed) => {
    const fake = fakeSession();
    const wire = await openWire(compressed, fake);
    const frames = [
      ' { "data": {}, "type": "Fu\\u006cl" } ',
      '{"data":{},"type":"Delta"}',
    ].map((text) => text.padEnd(gzipFixtureLength, " "));
    try {
      for (const text of frames) await wire.deliver(text);
      await vi.waitFor(() => expect(wire.received).toEqual([DECODED_PONG, DECODED_PONG]));
      expect(fake.decode.mock.calls.map(([frame]) => new TextDecoder().decode(frame))).toEqual(frames);
      expect(wire.recovery).toEqual([]);
    } finally {
      wire.socket.close();
    }
  });
});

describe("openPhaseSocket — codec lease ownership across preflight and send failure", () => {
  it.each(["resolve", "reject"] as const)(
    "releases the prepared lease once when LAN approval late-%ss after abort",
    async (completion) => {
      lanSupported.mockReturnValue(true); probeLan.mockResolvedValue(true);
      let complete!: () => void;
      const approval = new Promise<void>((resolve, reject) => {
        complete = () => completion === "resolve" ? resolve() : reject(new Error("Late approval failure"));
      });
      authorizeLan.mockReturnValue(approval);
      const fake = fakeSession();
      const controller = new AbortController();
      const pending = openPhaseSocket("ws://192.168.1.2:9374/ws", {
        signal: controller.signal,
        stateCodecSession: fake.session,
      });
      await vi.waitFor(() => expect(authorizeLan).toHaveBeenCalledOnce());
      controller.abort();
      await expect(pending).rejects.toMatchObject({ kind: "aborted" });
      complete();
      await approval.catch(() => {});
      await Promise.resolve();
      expect(fake.close).toHaveBeenCalledOnce();
      expect(MockWebSocket.instances).toHaveLength(0);
    },
  );

  it("releases the prepared lease once when LAN preflight fails", async () => {
    lanSupported.mockReturnValue(true); probeLan.mockResolvedValue(true);
    authorizeLan.mockRejectedValue(new Error("LAN approval denied"));
    const fake = fakeSession();
    await expect(openPhaseSocket("ws://192.168.1.2:9374/ws", {
      stateCodecSession: fake.session,
    })).rejects.toThrow("LAN approval denied");
    expect(fake.close).toHaveBeenCalledOnce();
    expect(MockWebSocket.instances).toHaveLength(0);
  });

  it("hands one lease through the recursive preflight call and closes it once", async () => {
    lanSupported.mockReturnValue(true); probeLan.mockResolvedValue(true);
    const fake = fakeSession();
    const pending = openPhaseSocket("ws://192.168.1.2:9374/ws", {
      stateCodecSession: fake.session,
      stateCodecBootstrap: () => "accepted",
    });
    await vi.waitFor(() => expect(channelListener.current).not.toBeNull());
    channelListener.current?.({ type: "message", text: fullHello({ mode: "Full" }) });
    // The recursive call owns the lease; negotiation wraps it for this socket.
    channelListener.current?.({
      type: "message",
      text: confirmationFrame(),
    });
    const socket = await pending;
    socket.ws.close();
    await vi.waitFor(() => expect(fake.close).toHaveBeenCalledOnce());
    expect(fake.close).toHaveBeenCalledOnce();
  });

  it("releases the prepared lease when the ClientHello send throws", async () => {
    const fake = fakeSession();
    const promise = openPhaseSocket("ws://test", {
      stateCodecSession: fake.session,
      stateCodecBootstrap: () => "accepted",
    });
    const raw = MockWebSocket.instances[0];
    raw.send.mockImplementationOnce(() => {
      throw new Error("socket closed before handshake hello");
    });
    raw.deliverMessage(fullHello());

    await expect(promise).rejects.toMatchObject({ kind: "ws_error" });
    expect(fake.close).toHaveBeenCalledOnce();
  });
});

describe("codec attachment and deadline boundaries", () => {
  it.each([false, true])("preserves immediate handoff and exact bytes (gzip=%s)", async (gzip) => {
    const text = ' { "type" : "Full", "data" : {"unicode":"雪 🧙"} } ';
    const fake = fakeSession();
    const sent = vi.fn();
    const pending = openPhaseSocket("ws://test", {
      stateCodecSession: fake.session, stateCodecBootstrap: () => "accepted", onFrameSent: sent,
    });
    const raw = MockWebSocket.instances[0];
    raw.send.mockImplementationOnce(() => {
      raw.deliverMessage(confirmationFrame());
      raw.deliverMessage(gzip ? new Uint8Array([0, ...new TextEncoder().encode(text)]) : text);
    });
    raw.deliverMessage(fullHello({ wire_formats: gzip ? ["GzipEnvelopeV1"] : [] }));
    const socket = await pending;
    const received = vi.fn();
    socket.ws.onmessage = received;
    await vi.waitFor(() => expect(received).toHaveBeenCalledOnce());
    expect(received.mock.calls[0][0].data).toBe(DECODED_PONG);
    expect(new TextDecoder().decode(fake.decode.mock.calls[0][0])).toBe(text);
    socket.ws.send('{"type":"Ping"}');
    await vi.waitFor(() => expect(sent).toHaveBeenCalledTimes(2));
    socket.close();
    expect(fake.close).toHaveBeenCalledOnce();
  });

  it("retains an early fatal close until the close handler attaches, even if it throws", async () => {
    vi.useFakeTimers();
    try {
      const fake = fakeSession();
      const pending = openPhaseSocket("ws://test", { stateCodecSession: fake.session });
      const raw = MockWebSocket.instances[0];
      raw.deliverMessage(fullHello());
      const socket = await pending;
      // The early fatal close is a codec frame ahead of the confirmation: the
      // confirmation has no deadline, and this failure needs no consumer.
      raw.deliverMessage(codecFrame("Full"));
      await vi.advanceTimersByTimeAsync(0);
      // Reach guard: the transport failed and released before any handler attached.
      expect(fake.close).toHaveBeenCalledOnce();
      expect(raw.close).toHaveBeenCalledOnce();
      const closed = vi.fn(() => { throw new Error("consumer"); });
      socket.ws.onclose = closed;
      expect(closed).toHaveBeenCalledOnce();
      expect(raw.close).toHaveBeenCalledOnce();
      expect(fake.close).toHaveBeenCalledOnce();
      raw.close();
      expect(closed).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it.each(["Error", "StateWithheld", "no response", "hung Full"] as const)(
    "keeps a fixed recovery deadline with %s", async (mode) => {
      vi.useFakeTimers();
      try {
        const fake = fakeSession(() => ({ kind: "resyncRequired" }));
        const recovery = vi.fn();
        const pending = openPhaseSocket("ws://test", {
          stateCodecSession: fake.session, stateCodecBootstrap: () => "accepted",
          onStateCodecRecovery: recovery,
        });
        const raw = MockWebSocket.instances[0];
        raw.deliverMessage(fullHello());
        const socket = await pending;
        const received = vi.fn();
        const closed = vi.fn();
        socket.ws.onmessage = received;
        socket.ws.onclose = closed;
        raw.deliverMessage(confirmationFrame());
        raw.deliverMessage(codecFrame("Delta"));
        await vi.advanceTimersByTimeAsync(0);
        expect(recovery).toHaveBeenCalledWith("recovering");
        expect(raw.send.mock.calls.filter(([text]) => text === '{"type":"ResyncState"}')).toHaveLength(1);
        await vi.advanceTimersByTimeAsync(4999);
        if (mode === "hung Full") {
          fake.decode.mockImplementationOnce(() => new Promise(() => undefined));
          raw.deliverMessage(codecFrame("Full"));
        } else if (mode !== "no response") {
          const notice = JSON.stringify({ type: mode, data: { message: "notice" } });
          raw.deliverMessage(notice);
          raw.deliverMessage(codecFrame("Delta"));
          raw.deliverMessage(codecFrame("Delta"));
          await vi.advanceTimersByTimeAsync(0);
          expect(received).toHaveBeenCalledWith(expect.objectContaining({ data: notice }));
        }
        await vi.advanceTimersByTimeAsync(1);
        expect(closed).toHaveBeenCalledOnce();
        expect(fake.close).toHaveBeenCalledOnce();
        expect(recovery).not.toHaveBeenCalledWith("ready");
      } finally {
        vi.useRealTimers();
      }
    },
  );

  it("delivers queued bootstrap before recovery and clears the deadline after Full", async () => {
    vi.useFakeTimers();
    try {
      let bootstrap: "pending" | "accepted" = "pending";
      let calls = 0;
      const fake = fakeSession(() => ++calls === 2
        ? { kind: "resyncRequired" } : { kind: "decoded", json: DECODED_PONG });
      const recovery: StateCodecRecoveryNotice[] = [];
      const pending = openPhaseSocket("ws://test", {
        stateCodecSession: fake.session, stateCodecBootstrap: () => bootstrap,
        onStateCodecRecovery: (state) => recovery.push(state),
      });
      const raw = MockWebSocket.instances[0];
      raw.deliverMessage(fullHello());
      const socket = await pending;
      raw.deliverMessage(confirmationFrame());
      raw.deliverMessage(codecFrame("Full"));
      raw.deliverMessage(codecFrame("Delta"));
      await vi.advanceTimersByTimeAsync(0);
      expect(fake.decode).not.toHaveBeenCalled();
      const messages: string[] = [];
      socket.ws.onmessage = (event) => { messages.push(event.data); bootstrap = "accepted"; return true; };
      const closed = vi.fn();
      socket.ws.onclose = closed;
      await vi.advanceTimersByTimeAsync(0);
      expect(recovery).toEqual(["recovering"]);
      raw.deliverMessage(codecFrame("Full"));
      raw.deliverMessage(codecFrame("Delta"));
      await vi.advanceTimersByTimeAsync(0);
      expect(messages).toEqual([DECODED_PONG, DECODED_PONG, DECODED_PONG]);
      expect(recovery).toEqual(["recovering", "ready"]);
      await vi.advanceTimersByTimeAsync(5001);
      expect(closed).not.toHaveBeenCalled();
      socket.close();
    } finally {
      vi.useRealTimers();
    }
  });

  it("closes rather than clearing recovery when delivery resets bootstrap", async () => {
    let calls = 0;
    const fake = fakeSession(() => ++calls === 1
      ? { kind: "resyncRequired" } : { kind: "decoded", json: DECODED_PONG });
    let bootstrap: "pending" | "accepted" = "accepted";
    const recovery = vi.fn();
    const pending = openPhaseSocket("ws://test", {
      stateCodecSession: fake.session, stateCodecBootstrap: () => bootstrap,
      onStateCodecRecovery: recovery,
    });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(fullHello());
    const socket = await pending;
    const received = vi.fn(() => { bootstrap = "pending"; return true; });
    const closed = vi.fn();
    socket.ws.onmessage = received;
    socket.ws.onclose = closed;
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(codecFrame("Delta"));
    await vi.waitFor(() => expect(recovery).toHaveBeenCalledWith("recovering"));
    raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(received).toHaveBeenCalledOnce();
    expect(recovery).not.toHaveBeenCalledWith("ready");
    expect(raw.close).toHaveBeenCalledOnce();
  });

  it.each(["handler", "listener"] as const)("retains a local close for a late close-only %s once", async (kind) => {
    const fake = fakeSession();
    const pending = openPhaseSocket("ws://test", { stateCodecSession: fake.session });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(fullHello());
    const socket = await pending;
    socket.close();
    // Invalidation and lease release are immediate; only the notification is posted.
    expect(fake.close).toHaveBeenCalledOnce();
    expect(raw.close).toHaveBeenCalledOnce();
    // Let the posted notification find no consumer, so it is retained.
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    const closed = vi.fn();
    if (kind === "handler") socket.ws.onclose = closed;
    else socket.ws.addEventListener("close", closed);
    expect(closed).toHaveBeenCalledOnce();
    socket.ws.onclose = vi.fn();
    raw.close();
    await Promise.resolve();
    expect(closed).toHaveBeenCalledOnce();
    expect(socket.ws.onclose).not.toHaveBeenCalled();
  });

  it.each(["handler", "listener"] as const)("posts a local close to a %s attached in the same turn, never inside close()", async (kind) => {
    const fake = fakeSession();
    const pending = openPhaseSocket("ws://test", { stateCodecSession: fake.session });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(fullHello());
    const socket = await pending;
    const closed = vi.fn();
    if (kind === "handler") socket.ws.onclose = closed;
    else socket.ws.addEventListener("close", closed);
    socket.close();
    // Native and gzip sockets deliver close asynchronously; so does the codec.
    expect(closed).not.toHaveBeenCalled();
    expect(fake.close).toHaveBeenCalledOnce();
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    // A later native close cannot notify a second time.
    raw.close();
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(closed).toHaveBeenCalledOnce();
  });

  it("does not publish ready or decode queued frames after recovery delivery closes the socket", async () => {
    let calls = 0;
    const fake = fakeSession(() => ++calls === 1
      ? { kind: "resyncRequired" } : { kind: "decoded", json: DECODED_PONG });
    const recovery = vi.fn();
    const pending = openPhaseSocket("ws://test", {
      stateCodecSession: fake.session, stateCodecBootstrap: () => "accepted",
      onStateCodecRecovery: recovery,
    });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(fullHello());
    const socket = await pending;
    const closed = vi.fn();
    socket.ws.onmessage = () => socket.close();
    socket.ws.onclose = closed;
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(codecFrame("Delta"));
    await vi.waitFor(() => expect(recovery).toHaveBeenCalledWith("recovering"));
    raw.deliverMessage(codecFrame("Full"));
    raw.deliverMessage(codecFrame("Delta"));
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(fake.decode).toHaveBeenCalledTimes(2);
    expect(fake.close).toHaveBeenCalledOnce();
    expect(recovery.mock.calls).toEqual([["recovering"]]);
  });
});

describe("StateCodecAttachment", () => {
  const socketA = { id: "a" } as unknown as PhaseSocketTransport;
  const socketB = { id: "b" } as unknown as PhaseSocketTransport;

  function adopted() {
    const attachment = new StateCodecAttachment();
    const binding = attachment.begin();
    attachment.adopt(binding, socketA);
    return { attachment, binding };
  }

  it("reports pending until a genuine GameStarted is accepted on the current socket", () => {
    const { attachment, binding } = adopted();
    expect(attachment.status(binding, socketA, false)).toBe("pending");
    attachment.accept(socketB);
    expect(attachment.status(binding, socketA, false)).toBe("pending");
    attachment.accept(socketA);
    expect(attachment.status(binding, socketA, false)).toBe("accepted");
    attachment.reset(socketA);
    expect(attachment.status(binding, socketA, false)).toBe("pending");
  });

  it("never lets a superseded, retired or disposed binding report accepted", () => {
    const { attachment, binding } = adopted();
    attachment.accept(socketA);
    expect(attachment.status(binding, socketA, false)).toBe("accepted");
    expect(attachment.status(binding, socketB, false)).toBe("pending");
    expect(attachment.status(binding, socketA, true)).toBe("pending");
    attachment.adopt(attachment.begin(), socketB);
    expect(attachment.status(binding, socketA, false)).toBe("pending");
  });

  it("marks recovery before interrupting the pending command and gates submissions until ready", () => {
    const { attachment, binding } = adopted();
    const interrupted: string[] = [];
    const blockedDuringInterrupt: boolean[] = [];
    attachment.handleRecovery(binding, "recovering", socketA, false, (error) => {
      interrupted.push(`${error.code}:${error.recoverable}`);
      blockedDuringInterrupt.push(blocked(attachment));
    });
    expect(interrupted).toEqual(["WS_CLOSED:true"]);
    expect(blockedDuringInterrupt).toEqual([true]);
    expect(blocked(attachment)).toBe(true);
    attachment.handleRecovery(binding, "ready", socketA, false, () => undefined);
    expect(blocked(attachment)).toBe(false);
  });

  it("ignores recovery notifications from a retired binding, a retired socket or a disposed adapter", () => {
    const { attachment, binding } = adopted();
    const interrupt = vi.fn();
    attachment.handleRecovery(binding, "recovering", socketB, false, interrupt);
    attachment.handleRecovery(binding, "recovering", socketA, true, interrupt);
    attachment.handleRecovery(attachment.begin(), "recovering", socketA, false, interrupt);
    expect(interrupt).not.toHaveBeenCalled();
    expect(blocked(attachment)).toBe(false);
    // Reach guard: the same call on the live binding is honoured.
    attachment.handleRecovery(binding, "recovering", socketA, false, interrupt);
    expect(interrupt).toHaveBeenCalledOnce();
    expect(blocked(attachment)).toBe(true);
  });

  it("clears the recovery gate on adoption of a replacement, on clearRecovery and on release", () => {
    const { attachment, binding } = adopted();
    attachment.handleRecovery(binding, "recovering", socketA, false, () => undefined);
    attachment.adopt(attachment.begin(), socketB);
    expect(blocked(attachment)).toBe(false);

    const second = adopted();
    second.attachment.handleRecovery(second.binding, "recovering", socketA, false, () => undefined);
    second.attachment.clearRecovery();
    expect(blocked(second.attachment)).toBe(false);

    const third = adopted();
    third.attachment.handleRecovery(third.binding, "recovering", socketA, false, () => undefined);
    // Accepted first, so only a release that really drops the binding can
    // turn the status back to pending.
    third.attachment.accept(socketA);
    expect(third.attachment.status(third.binding, socketA, false)).toBe("accepted");
    third.attachment.release();
    expect(blocked(third.attachment)).toBe(false);
    expect(third.attachment.status(third.binding, socketA, false)).toBe("pending");
  });

  it("selects the provider and lease by policy, and the first failure on the current attachment wins", () => {
    const { attachment, binding } = adopted();
    const provider = vi.fn();
    const lease = { decode: vi.fn(), close: vi.fn() };
    // Reach guard: while negotiating, both selections pass through.
    expect(attachment.deliveryPolicy).toEqual({ kind: "negotiate" });
    expect(attachment.selectProvider(provider)).toBe(provider);
    expect(attachment.selectProvider(null)).toBeNull();
    expect(attachment.selectLease(lease)).toBe(lease);
    expect(lease.close).not.toHaveBeenCalled();

    attachment.handleRecovery(binding, { kind: "failed", reason: "repeatedRecovery", joinCredentials: null }, socketA, false, vi.fn());
    attachment.handleRecovery(binding, { kind: "failed", reason: "decodeFailure", joinCredentials: null }, socketA, false, vi.fn());
    expect(attachment.deliveryPolicy).toEqual({ kind: "snapshotOnly", reason: "repeatedRecovery" });
    expect(attachment.selectProvider(provider)).toBeNull();
    expect(attachment.selectLease(lease)).toBeNull();
    expect(lease.close).toHaveBeenCalledOnce();
    expect(attachment.selectLease(null)).toBeNull();
  });

  it("never resets the policy on begin, adopt, accept, reset, recovery completion or clearRecovery, only on release", () => {
    const { attachment, binding } = adopted();
    attachment.handleRecovery(binding, { kind: "failed", reason: "confirmationFailure", joinCredentials: null }, socketA, false, vi.fn());
    const degraded = { kind: "snapshotOnly", reason: "confirmationFailure" };
    expect(attachment.deliveryPolicy).toEqual(degraded);

    attachment.accept(socketA);
    attachment.reset(socketA);
    attachment.handleRecovery(binding, "recovering", socketA, false, vi.fn());
    attachment.handleRecovery(binding, "ready", socketA, false, vi.fn());
    attachment.clearRecovery();
    attachment.adopt(attachment.begin(), socketB);
    expect(attachment.deliveryPolicy).toEqual(degraded);

    attachment.release();
    expect(attachment.deliveryPolicy).toEqual({ kind: "negotiate" });
  });

  it("does not let a retired, foreign-socket, disposed or still-pending binding degrade the policy", () => {
    const { attachment, binding } = adopted();
    const fail = { kind: "failed", reason: "decodeFailure", joinCredentials: null } as const;
    // A failure on a socket that is not the current one, and a disposed adapter.
    attachment.handleRecovery(binding, fail, socketB, false, vi.fn());
    expect(attachment.deliveryPolicy).toEqual({ kind: "negotiate" });
    const disposedBinding = attachment.begin();
    attachment.handleRecovery(disposedBinding, fail, socketA, true, vi.fn());
    expect(disposedBinding.failure).toBeNull();
    // A still-pending replacement records its reason but cannot degrade early.
    const pending = attachment.begin();
    attachment.handleRecovery(pending, fail, socketA, false, vi.fn());
    expect(pending.failure).toEqual(fail);
    expect(attachment.deliveryPolicy).toEqual({ kind: "negotiate" });
    // Retired by the adoption of a newer binding, the old one never degrades.
    attachment.adopt(attachment.begin(), socketB);
    attachment.handleRecovery(binding, fail, socketB, false, vi.fn());
    expect(attachment.deliveryPolicy).toEqual({ kind: "negotiate" });
    // Reach guard: the same notice on the live binding does degrade.
    const live = attachment.begin();
    attachment.adopt(live, socketA);
    attachment.handleRecovery(live, fail, socketA, false, vi.fn());
    expect(attachment.deliveryPolicy).toEqual({ kind: "snapshotOnly", reason: "decodeFailure" });
  });

  it("consumes a reason captured before adoption and keeps the first reason on a binding", () => {
    const attachment = new StateCodecAttachment();
    const early = attachment.begin();
    const first = { kind: "failed", reason: "confirmationFailure", joinCredentials: null } as const;
    attachment.handleRecovery(early, first, null, false, vi.fn());
    attachment.handleRecovery(
      early,
      { kind: "failed", reason: "decodeFailure", joinCredentials: null },
      null,
      false,
      vi.fn(),
    );
    expect(early.failure).toEqual(first);
    expect(attachment.deliveryPolicy).toEqual({ kind: "negotiate" });
    attachment.adopt(early, socketA);
    expect(attachment.deliveryPolicy).toEqual({ kind: "snapshotOnly", reason: "confirmationFailure" });
    // A failure does not touch the recovery gate that the close settles.
    expect(blocked(attachment)).toBe(false);
  });

  function blocked(attachment: StateCodecAttachment): boolean {
    try {
      attachment.assertNotRecovering();
      return false;
    } catch (error) {
      expect((error as { code?: string }).code).toBe("ACTION_NOT_SENT");
      return true;
    }
  }
});

// ── Typed terminal codec reasons ────────────────────────────────────────
//
// Every codec-specific fatal close publishes exactly one typed `failed` notice,
// after the transport is invalidated and before the close notification. The
// mutual advertisement in each row is the positive reach guard.

describe("openPhaseSocket — typed terminal codec reasons", () => {
  type Trace = string[];

  async function open(
    fake: ReturnType<typeof fakeSession>,
    options: {
      bootstrap?: () => "pending" | "accepted";
      onRecovery?: (state: StateCodecRecoveryNotice) => void;
      timeoutMs?: number;
      attachHandlers?: boolean;
    } = {},
  ) {
    const notices: StateCodecRecoveryNotice[] = [];
    const trace: Trace = [];
    const promise = openPhaseSocket("ws://test", {
      stateCodecSession: fake.session,
      stateCodecBootstrap: options.bootstrap ?? (() => "accepted"),
      timeoutMs: options.timeoutMs,
      onStateCodecRecovery: (state) => {
        notices.push(state);
        trace.push(typeof state === "string" ? state : `failed:${state.reason}`);
        options.onRecovery?.(state);
      },
    });
    const raw = MockWebSocket.instances[0];
    raw.deliverMessage(fullHello());
    const socket = await promise;
    // Reach guard: the codec was really advertised, so the wrapper exists.
    expect(raw.send).toHaveBeenCalledWith(expect.stringContaining('"state_codecs":["StateCodecV1"]'));
    const closed = vi.fn(() => { trace.push("close"); });
    const received = vi.fn((event: { data: string }) => {
      trace.push(`message:${event.data.length}`);
      return true;
    });
    if (options.attachHandlers !== false) {
      socket.ws.onclose = closed;
      socket.ws.onmessage = received;
    }
    return { raw, socket, notices, trace, closed, received };
  }

  const ping = (n: number) => JSON.stringify({ type: "Pong", data: { timestamp: n } });

  it("reports decodeFailure before the close and never resyncs", async () => {
    const fake = fakeSession();
    fake.decode.mockImplementationOnce(async () => { throw new Error("State codec decode timed out"); });
    const { raw, notices, trace, closed } = await open(fake);
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(notices).toEqual([{ kind: "failed", reason: "decodeFailure", joinCredentials: null }]);
    expect(trace).toEqual(["failed:decodeFailure", "close"]);
    expect(fake.close).toHaveBeenCalledOnce();
    expect(raw.send).not.toHaveBeenCalledWith('{"type":"ResyncState"}');
    // Later frames cannot publish another notice (the decoder would accept them).
    raw.deliverMessage(codecFrame("Delta"));
    raw.deliverMessage(ping(1));
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(notices).toHaveLength(1);
  });

  it.each([
    ["a contradictory selection", () => confirmationFrame("OtherCodec")],
    ["a missing selection", () => JSON.stringify({ type: "ServerHello", data: { server_version: "x" } })],
  ])("reports confirmationFailure for %s", async (_label, frame) => {
    const fake = fakeSession();
    const { raw, notices, closed } = await open(fake);
    raw.deliverMessage(frame());
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(notices).toEqual([{ kind: "failed", reason: "confirmationFailure", joinCredentials: null }]);
    expect(fake.close).toHaveBeenCalledOnce();
  });

  it("reports confirmationFailure when a codec frame precedes the confirmation, even with no consumer attached", async () => {
    const fake = fakeSession();
    const { raw, notices, socket, trace } = await open(fake, { attachHandlers: false });
    raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(notices).toEqual([{ kind: "failed", reason: "confirmationFailure", joinCredentials: null }]));
    expect(fake.decode).not.toHaveBeenCalled();
    expect(fake.close).toHaveBeenCalledOnce();
    // The close is retained for the consumer that attaches later, and still last.
    const closed = vi.fn(() => { trace.push("close"); });
    socket.ws.onclose = closed;
    expect(closed).toHaveBeenCalledOnce();
    expect(trace).toEqual(["failed:confirmationFailure", "close"]);
  });

  it("reports no failure while the confirmation is merely slow, and still reports a violation after the wait", async () => {
    vi.useFakeTimers();
    try {
      const fake = fakeSession();
      const { raw, notices, closed } = await open(fake, { timeoutMs: 1000 });
      await vi.advanceTimersByTimeAsync(10_000);
      expect(notices).toEqual([]);
      expect(closed).not.toHaveBeenCalled();
      expect(fake.close).not.toHaveBeenCalled();
      // Reach guard: the transport is still live, so the silence above is real.
      raw.deliverMessage(codecFrame("Full"));
      await vi.advanceTimersByTimeAsync(0);
      expect(notices).toEqual([{ kind: "failed", reason: "confirmationFailure", joinCredentials: null }]);
      expect(closed).toHaveBeenCalledOnce();
      expect(fake.close).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it("reports bootstrapRejection even when the best-effort resync cannot be sent", async () => {
    let calls = 0;
    const fake = fakeSession(() => ++calls === 1 ? { kind: "resyncRequired" } : { kind: "decoded", json: DECODED_PONG });
    const { raw, notices, closed } = await open(fake, { bootstrap: () => "pending" });
    raw.deliverMessage(confirmationFrame());
    raw.send.mockImplementation(() => { throw new Error("send failed"); });
    raw.deliverMessage(codecFrame("Delta"));
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(notices).toEqual(["recovering", { kind: "failed", reason: "bootstrapRejection", joinCredentials: null }]);
    expect(raw.send).toHaveBeenCalledWith('{"type":"ResyncState"}');
  });

  it("does not report a codec reason when the resync of an established game cannot be sent", async () => {
    const fake = fakeSession(() => ({ kind: "resyncRequired" }));
    const { raw, notices, closed } = await open(fake);
    raw.deliverMessage(confirmationFrame());
    raw.send.mockImplementation(() => { throw new Error("send failed"); });
    raw.deliverMessage(codecFrame("Delta"));
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(raw.send).toHaveBeenCalledWith('{"type":"ResyncState"}');
    expect(notices).toEqual(["recovering"]);
  });

  it("reports recoveryFullRejection when the recovery Full is rejected, once and before the close", async () => {
    const fake = fakeSession(() => ({ kind: "resyncRequired" }));
    const { raw, notices, trace, closed } = await open(fake);
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(codecFrame("Delta"));
    await vi.waitFor(() => expect(notices).toEqual(["recovering"]));
    // A Delta inside the window is suppressed, not a rejected recovery Full.
    raw.deliverMessage(codecFrame("Delta"));
    raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(fake.decode).toHaveBeenCalledTimes(2);
    expect(trace).toEqual(["recovering", "failed:recoveryFullRejection", "close"]);
  });

  it("reports recoveryTimeout independently of a hung recovery decode", async () => {
    vi.useFakeTimers();
    try {
      let calls = 0;
      const fake = fakeSession();
      fake.decode.mockImplementation(async (): Promise<StateCodecDecodeResult> => {
        calls += 1;
        if (calls === 1) return { kind: "resyncRequired" };
        return new Promise(() => undefined);
      });
      const { raw, notices, closed } = await open(fake, { timeoutMs: 1000 });
      raw.deliverMessage(confirmationFrame());
      raw.deliverMessage(codecFrame("Delta"));
      await vi.advanceTimersByTimeAsync(0);
      expect(notices).toEqual(["recovering"]);
      raw.deliverMessage(codecFrame("Full"));
      await vi.advanceTimersByTimeAsync(0);
      // Reach guard: the recovery Full really is stuck inside the decoder.
      expect(fake.decode).toHaveBeenCalledTimes(2);
      expect(closed).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(1001);
      expect(notices).toEqual(["recovering", { kind: "failed", reason: "recoveryTimeout", joinCredentials: null }]);
      expect(closed).toHaveBeenCalledOnce();
      expect(fake.close).toHaveBeenCalledOnce();
      expect(raw.send.mock.calls.filter(([v]) => v === '{"type":"ResyncState"}')).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("tolerates one completed recovery and reports repeatedRecovery on the second, after delivering its Full", async () => {
    // Frame script: 1 Full ok, 2 Delta rejected, 3 Full ok (recovery 1),
    // 4 Delta ok (healthy), 5 Delta rejected, 6 Full ok (recovery 2).
    let calls = 0;
    const rejected = new Set([2, 5]);
    const fake = fakeSession(() => ++calls && rejected.has(calls)
      ? { kind: "resyncRequired" } : { kind: "decoded", json: DECODED_PONG });
    const { raw, notices, trace, closed } = await open(fake);
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(codecFrame("Full"));
    raw.deliverMessage(codecFrame("Delta"));
    await vi.waitFor(() => expect(notices).toEqual(["recovering"]));
    raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(notices).toEqual(["recovering", "ready"]));
    // Reach guard: the first recovery completed without closing.
    expect(closed).not.toHaveBeenCalled();
    raw.deliverMessage(codecFrame("Delta"));
    raw.deliverMessage(codecFrame("Delta"));
    await vi.waitFor(() => expect(notices).toEqual(["recovering", "ready", "recovering"]));
    expect(fake.decode).toHaveBeenCalledTimes(5);
    raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    // The second recovery's Full is delivered, never published as ready, and
    // the typed reason precedes the close.
    expect(notices).toEqual(["recovering", "ready", "recovering", { kind: "failed", reason: "repeatedRecovery", joinCredentials: null }]);
    expect(trace.slice(-3)).toEqual([expect.stringMatching(/^message:/), "failed:repeatedRecovery", "close"]);
    expect(fake.close).toHaveBeenCalledOnce();
    expect(raw.send.mock.calls.filter(([v]) => v === '{"type":"ResyncState"}')).toHaveLength(2);
  });

  it("does not report a codec reason for a network close, a healthy stream or an identity close during recovery delivery", async () => {
    const fake = fakeSession();
    const network = await open(fake);
    network.raw.deliverMessage(confirmationFrame());
    network.raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(network.received).toHaveBeenCalledOnce());
    network.raw.close();
    await vi.waitFor(() => expect(network.closed).toHaveBeenCalledOnce());
    expect(network.notices).toEqual([]);

    // The application resets bootstrap while the recovery Full is delivered.
    MockWebSocket.instances.length = 0;
    let calls = 0;
    let bootstrap: "pending" | "accepted" = "accepted";
    const lost = fakeSession(() => ++calls === 1
      ? { kind: "resyncRequired" } : { kind: "decoded", json: DECODED_PONG });
    const reset = await open(lost, { bootstrap: () => bootstrap });
    reset.raw.deliverMessage(confirmationFrame());
    reset.raw.deliverMessage(codecFrame("Delta"));
    await vi.waitFor(() => expect(reset.notices).toEqual(["recovering"]));
    reset.socket.ws.onmessage = () => { bootstrap = "pending"; return true; };
    reset.raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(reset.closed).toHaveBeenCalledOnce());
    expect(reset.notices).toEqual(["recovering"]);
  });

  it("completes the first recovery across Error and StateWithheld frames", async () => {
    let calls = 0;
    const fake = fakeSession(() => ++calls === 1
      ? { kind: "resyncRequired" } : { kind: "decoded", json: DECODED_PONG });
    const { raw, notices, closed, received } = await open(fake);
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(codecFrame("Delta"));
    await vi.waitFor(() => expect(notices).toEqual(["recovering"]));
    raw.deliverMessage(JSON.stringify({ type: "Error", data: { message: "x" } }));
    raw.deliverMessage(JSON.stringify({ type: "StateWithheld", data: {} }));
    await vi.waitFor(() => expect(received).toHaveBeenCalledTimes(2));
    raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(notices).toEqual(["recovering", "ready"]));
    expect(closed).not.toHaveBeenCalled();
  });

  it("publishes the failed notice before the close even when the failed callback throws", async () => {
    const fake = fakeSession();
    fake.decode.mockImplementationOnce(async () => { throw new Error("decode failed"); });
    const { raw, closed, notices } = await open(fake, {
      onRecovery: () => { throw new Error("callback"); },
    });
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(notices).toEqual([{ kind: "failed", reason: "decodeFailure", joinCredentials: null }]);
    expect(raw.close).toHaveBeenCalledOnce();
    expect(fake.close).toHaveBeenCalledOnce();
  });

  it("reports nothing for a local close followed by a late decode rejection", async () => {
    let reject!: (error: Error) => void;
    const fake = fakeSession();
    fake.decode.mockImplementationOnce(() => new Promise((_resolve, rej) => { reject = rej; }));
    const { raw, socket, notices } = await open(fake);
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(fake.decode).toHaveBeenCalledOnce());
    socket.close();
    reject(new Error("late failure"));
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(notices).toEqual([]);
    expect(fake.close).toHaveBeenCalledOnce();
  });
});

// ── Ordered codec errors and typed join credentials ─────────────────────
//
// A raw transport error is only a notification: it is ordered behind every frame
// the codec wrapper has already received, and ahead of the natural close. A
// codec failure that happens while the application bootstrap is pending carries
// the join credentials of the exact GameStarted `Full` that failed, and nothing
// else ever does.

const GAME_STARTED_CREDENTIALS = {
  playerToken: "join-token",
  yourPlayer: 1,
  fullKey: { game_code: "GAME01", generation: 3 },
} as const;

/** A `Full` frame whose state is a GameStarted envelope, as the server serializes it. */
function gameStartedFullFrame(
  started: Record<string, unknown> = {},
  envelope: Record<string, unknown> = {},
): string {
  return JSON.stringify({
    type: "Full",
    data: {
      state: {
        type: "GameStarted",
        data: {
          state: { turn_number: 1 },
          state_revision: 1,
          player_token: "join-token",
          your_player: 1,
          full_key: { game_code: "GAME01", generation: 3 },
          ...started,
        },
      },
      revision: 1,
      ...envelope,
    },
  });
}

const gzipFrame = (text: string) => new Uint8Array([0, ...new TextEncoder().encode(text)]);
const drainMacrotask = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

/** A session whose first decode stays inside the decoder until `release` is called. */
function heldDecoder() {
  const fake = fakeSession();
  let resolveDecode: ((result: StateCodecDecodeResult) => void) | null = null;
  let rejectDecode: ((error: Error) => void) | null = null;
  let signalEntered!: () => void;
  const entered = new Promise<void>((resolve) => { signalEntered = resolve; });
  fake.decode.mockImplementationOnce(
    () => new Promise<StateCodecDecodeResult>((resolve, reject) => {
      resolveDecode = resolve;
      rejectDecode = reject;
      signalEntered();
    }),
  );
  return {
    fake,
    entered,
    release: (result: StateCodecDecodeResult) => {
      if (resolveDecode === null) throw new Error("the decoder was never entered");
      resolveDecode(result);
    },
    reject: (error: Error) => {
      if (rejectDecode === null) throw new Error("the decoder was never entered");
      rejectDecode(error);
    },
  };
}

async function openCodecSocket(
  fake: ReturnType<typeof fakeSession>,
  options: {
    bootstrap?: () => "pending" | "accepted";
    onRecovery?: (state: StateCodecRecoveryNotice) => void;
    gzip?: boolean;
    beforeAdoption?: () => void;
  } = {},
) {
  const promise = openPhaseSocket("ws://test", {
    stateCodecSession: fake.session,
    stateCodecBootstrap: options.bootstrap ?? (() => "accepted"),
    onStateCodecRecovery: options.onRecovery,
  });
  const raw = MockWebSocket.instances[0];
  options.beforeAdoption?.();
  raw.deliverMessage(fullHello({ wire_formats: options.gzip === true ? ["GzipEnvelopeV1"] : [] }));
  const socket = await promise;
  // Reach guard: the codec really was advertised, so the wrapper exists.
  expect(raw.send).toHaveBeenCalledWith(expect.stringContaining('"state_codecs":["StateCodecV1"]'));
  return { raw, socket };
}

describe("openPhaseSocket — natural close during codec decode", () => {
  // Network teardown may drain already-received healthy frames for at most
  // five seconds. This is a close bound, not the decoder's own RPC watchdog.
  const orderedDrainBoundMs = 5_000;

  async function heldAttachment(wire: "raw" | "gzip", beforeAdoption?: () => void) {
    const held = heldDecoder();
    const attachment = new StateCodecAttachment();
    const binding = attachment.begin();
    const notices: StateCodecRecoveryNotice[] = [];
    let adopted: PhaseSocketTransport | null = null;
    const { raw, socket } = await openCodecSocket(held.fake, {
      gzip: wire === "gzip",
      bootstrap: () => "pending",
      beforeAdoption,
      onRecovery: (state) => {
        notices.push(state);
        attachment.handleRecovery(binding, state, adopted, false, vi.fn());
      },
    });
    adopted = socket.ws;
    attachment.adopt(binding, socket.ws);
    const received = vi.fn();
    const closed = vi.fn();
    const closeListener = vi.fn();
    socket.ws.onmessage = received;
    socket.ws.onclose = closed;
    socket.ws.addEventListener("close", closeListener, { once: true });
    raw.deliverMessage(confirmationFrame());
    const text = gameStartedFullFrame({ padding: "x".repeat(WIRE_COMPRESSION_THRESHOLD) });
    if (wire === "gzip") {
      const compressed = await encodeJsonEnvelope(text);
      // Exercise actual gzip inflation, not just the raw 0x00 envelope.
      expect(compressed[0]).toBe(0x01);
      raw.deliverMessage(compressed.buffer);
    } else {
      raw.deliverMessage(text);
    }
    await held.entered;
    expect(held.fake.decode).toHaveBeenCalledOnce();
    expect(new TextDecoder().decode(held.fake.decode.mock.calls[0][0])).toBe(text);
    return { held, attachment, notices, raw, socket, received, closed, closeListener };
  }

  /** Hold real inflater output, rather than delaying acquisition of the frame bytes. */
  function heldGzipInflation() {
    const NativeDecompressionStream = globalThis.DecompressionStream;
    const started = vi.fn();
    let signalEntered!: () => void;
    let signalFinished!: () => void;
    let release!: () => void;
    let reject!: (error: Error) => void;
    const entered = new Promise<void>((resolve) => { signalEntered = resolve; });
    const finished = new Promise<void>((resolve) => { signalFinished = resolve; });
    const gate = new Promise<void>((resolve, rejectGate) => {
      release = resolve;
      reject = rejectGate;
    });
    vi.stubGlobal("DecompressionStream", class {
      readonly readable: ReadableStream<Uint8Array>;
      readonly writable: DecompressionStream["writable"];

      constructor(format: CompressionFormat) {
        started(format);
        const inflater = new NativeDecompressionStream(format);
        this.writable = inflater.writable;
        this.readable = inflater.readable.pipeThrough(new TransformStream<Uint8Array, Uint8Array>({
          async transform(chunk, controller) {
            // Real gzip bytes have already reached the native inflater. Keep
            // wireEnvelope's Response.arrayBuffer() pending inside its stream.
            signalEntered();
            try {
              await gate;
              controller.enqueue(chunk);
            } catch (error) {
              signalFinished();
              throw error;
            }
          },
          flush() { signalFinished(); },
        }));
      }
    });
    return {
      entered, finished, release, reject, started,
      restore: () => { vi.stubGlobal("DecompressionStream", NativeDecompressionStream); },
    };
  }

  async function pendingGzipAttachment(beforeAdoption?: () => void) {
    const frameB = codecFrame("Delta").padEnd(WIRE_COMPRESSION_THRESHOLD + 1, " ");
    const frameC = JSON.stringify({
      type: "Delta", data: { state: { type: "Pong" }, revision: 2 },
    }).padEnd(WIRE_COMPRESSION_THRESHOLD + 1, " ");
    // Compression itself must finish before the controlled inflater or fake
    // clock is installed. A, B and C use genuine 0x01 gzip envelopes.
    const compressedB = await encodeJsonEnvelope(frameB);
    const compressedC = await encodeJsonEnvelope(frameC);
    expect(compressedB[0]).toBe(0x01);
    expect(compressedC[0]).toBe(0x01);
    const wire = await heldAttachment("gzip", beforeAdoption);
    expect(wire.raw.send).toHaveBeenCalledWith(expect.stringContaining('"wire_formats":["GzipEnvelopeV1"]'));
    const errored = vi.fn();
    const messageListener = vi.fn();
    wire.socket.ws.onerror = errored;
    wire.socket.ws.addEventListener("message", messageListener);
    // A persistent observer also detects an incorrectly repeated close.
    wire.socket.ws.removeEventListener("close", wire.closeListener);
    wire.socket.ws.addEventListener("close", wire.closeListener);
    const inflation = heldGzipInflation();
    return {
      ...wire, frameB, compressedB, frameC, compressedC, inflation, errored, messageListener,
      dispose: async () => {
        wire.held.release({ kind: "decoded", json: DECODED_PONG });
        inflation.release();
        try {
          await inflation.finished;
        } finally {
          inflation.restore();
          vi.useRealTimers();
          wire.socket.close();
        }
      },
    };
  }

  interface NaturalGzipCloseObservation {
    closed: Mock;
    closeListener: Mock;
    held: { fake: { close: Mock } };
    notices: StateCodecRecoveryNotice[];
    attachment: StateCodecAttachment;
    raw: MockWebSocket;
    errored: Mock;
    inflation: { started: Mock };
  }

  function expectOriginalGzipClose(
    wire: NaturalGzipCloseObservation,
    natural: CloseEvent,
    inflatedFrames = 1,
  ) {
    expect(wire.closed).toHaveBeenCalledOnce();
    expect(wire.closed.mock.calls[0][0]).toBe(natural);
    expect(wire.closeListener).toHaveBeenCalledOnce();
    expect(wire.closeListener.mock.calls[0][0]).toBe(natural);
    expect(wire.held.fake.close).toHaveBeenCalledOnce();
    expect(wire.notices).toEqual([]);
    expect(wire.attachment.deliveryPolicy).toEqual({ kind: "negotiate" });
    expect(wire.raw.send).not.toHaveBeenCalledWith('{"type":"ResyncState"}');
    expect(wire.raw.close).not.toHaveBeenCalled();
    expect(wire.errored).not.toHaveBeenCalled();
    expect(wire.inflation.started).toHaveBeenCalledTimes(inflatedFrames);
    expect(wire.inflation.started).toHaveBeenCalledWith("gzip");
  }

  it("discards queued real gzip C when decoder A rejects after raw close while inflater B is held", async () => {
    const wire = await pendingGzipAttachment();
    const { held, raw, socket, received, closed, inflation, messageListener } = wire;
    try {
      raw.deliverMessage(wire.compressedB.buffer);
      await inflation.entered;
      raw.deliverMessage(wire.compressedC.buffer);
      expect(inflation.started).toHaveBeenCalledExactlyOnceWith("gzip");
      expect(held.fake.decode).toHaveBeenCalledOnce();
      expect(received).not.toHaveBeenCalled();
      vi.useFakeTimers();
      const natural = new CloseEvent("close", { code: 1006, reason: "network lost during inflation", wasClean: false });
      raw.fireClose(natural);
      // Gzip cannot deliver its ordered close until B finishes inflation.
      expect(closed).not.toHaveBeenCalled();
      held.reject(new Error("A decoder rejected before B finished inflation"));
      await vi.advanceTimersByTimeAsync(0);
      expectOriginalGzipClose(wire, natural);
      expect(received).not.toHaveBeenCalled();
      expect(messageListener).not.toHaveBeenCalled();

      inflation.release();
      await inflation.finished;
      await vi.advanceTimersByTimeAsync(0);
      // B was already in the real inflater, but A's rejected drain ended the
      // connection. C was queued before raw death and must not even inflate.
      raw.deliverMessage(wire.compressedB.buffer);
      raw.fireError();
      raw.fireClose(natural);
      socket.close();
      await vi.advanceTimersByTimeAsync(orderedDrainBoundMs);
      expectOriginalGzipClose(wire, natural);
      expect(held.fake.decode).toHaveBeenCalledOnce();
      expect(received).not.toHaveBeenCalled();
      expect(messageListener).not.toHaveBeenCalled();
    } finally {
      await wire.dispose();
    }
  });

  it("preserves the original raw close when an earlier raw observer closes the codec during held gzip inflation", async () => {
    const closeFromRawObserver = vi.fn((_event: Event) => { closeAdopted(); });
    const wire = await pendingGzipAttachment(() => {
      // An existing application observer predates codec construction. Its
      // synchronous cleanup must not replace network death with a local close.
      MockWebSocket.instances[0].addEventListener("close", closeFromRawObserver, { once: true });
    });
    const { held, raw, socket, received, inflation, messageListener } = wire;
    const closeAdopted = () => socket.close();
    try {
      raw.deliverMessage(wire.compressedB.buffer);
      await inflation.entered;
      raw.deliverMessage(wire.compressedC.buffer);
      expect(inflation.started).toHaveBeenCalledExactlyOnceWith("gzip");
      vi.useFakeTimers();
      const natural = new CloseEvent("close", { code: 1006, reason: "network lost during reentrant cleanup" });
      raw.fireClose(natural);
      await vi.advanceTimersByTimeAsync(0);
      expect(closeFromRawObserver).toHaveBeenCalledOnce();
      expect(closeFromRawObserver.mock.calls[0][0]).toBe(natural);
      expectOriginalGzipClose(wire, natural);

      held.reject(new Error("A decoder rejected after the raw observer disposed the socket"));
      inflation.release();
      await inflation.finished;
      raw.deliverMessage(wire.compressedB.buffer);
      socket.close();
      await vi.advanceTimersByTimeAsync(orderedDrainBoundMs);
      expectOriginalGzipClose(wire, natural);
      expect(closeFromRawObserver).toHaveBeenCalledOnce();
      expect(held.fake.decode).toHaveBeenCalledOnce();
      expect(received).not.toHaveBeenCalled();
      expect(messageListener).not.toHaveBeenCalled();
    } finally {
      await wire.dispose();
    }
  });

  it("drains healthy real gzip A, B and queued C in order before the original network close", async () => {
    const wire = await pendingGzipAttachment();
    const { held, raw, received, closed, inflation, messageListener } = wire;
    const decodedB = '{"type":"Pong","data":{"timestamp":29}}';
    const decodedC = '{"type":"Pong","data":{"timestamp":30}}';
    let signalDeliveredC!: () => void;
    const deliveredC = new Promise<void>((resolve) => { signalDeliveredC = resolve; });
    const trace: string[] = [];
    held.fake.decode
      .mockResolvedValueOnce({ kind: "decoded", json: decodedB })
      .mockResolvedValueOnce({ kind: "decoded", json: decodedC });
    held.fake.close.mockImplementation(() => { trace.push("lease"); });
    received.mockImplementation((event: MessageEvent<string>) => {
      trace.push(event.data);
      if (event.data === decodedC) signalDeliveredC();
      return true;
    });
    closed.mockImplementation(() => { trace.push("close"); });
    try {
      raw.deliverMessage(wire.compressedB.buffer);
      await inflation.entered;
      raw.deliverMessage(wire.compressedC.buffer);
      expect(inflation.started).toHaveBeenCalledExactlyOnceWith("gzip");
      vi.useFakeTimers();
      const natural = new CloseEvent("close", { code: 1006, reason: "network lost during healthy drain" });
      raw.fireClose(natural);
      held.release({ kind: "decoded", json: DECODED_PONG });
      await vi.advanceTimersByTimeAsync(0);
      expect(trace).toEqual([DECODED_PONG]);
      expect(held.fake.decode).toHaveBeenCalledOnce();
      expect(held.fake.close).not.toHaveBeenCalled();
      expect(closed).not.toHaveBeenCalled();

      inflation.release();
      await inflation.finished;
      await deliveredC;
      await vi.advanceTimersByTimeAsync(0);
      expect(trace).toEqual([DECODED_PONG, decodedB, decodedC, "lease", "close"]);
      expect(held.fake.decode).toHaveBeenCalledTimes(3);
      expect(new TextDecoder().decode(held.fake.decode.mock.calls[1][0])).toBe(wire.frameB);
      expect(new TextDecoder().decode(held.fake.decode.mock.calls[2][0])).toBe(wire.frameC);
      expect(received.mock.calls.map(([event]) => event.data)).toEqual([DECODED_PONG, decodedB, decodedC]);
      expect(messageListener.mock.calls.map(([event]) => event.data)).toEqual([DECODED_PONG, decodedB, decodedC]);
      expectOriginalGzipClose(wire, natural, 2);
      await vi.advanceTimersByTimeAsync(orderedDrainBoundMs);
      expect(trace).toEqual([DECODED_PONG, decodedB, decodedC, "lease", "close"]);
      expectOriginalGzipClose(wire, natural, 2);
    } finally {
      await wire.dispose();
    }
  });

  it.each(["success", "failure"] as const)(
    "discards queued real gzip C at the raw-death deadline and fences held B's late %s",
    async (late) => {
      const wire = await pendingGzipAttachment();
      const { held, raw, socket, received, closed, inflation, messageListener } = wire;
      try {
        raw.deliverMessage(wire.compressedB.buffer);
        await inflation.entered;
        raw.deliverMessage(wire.compressedC.buffer);
        expect(inflation.started).toHaveBeenCalledExactlyOnceWith("gzip");
        vi.useFakeTimers();
        const natural = new CloseEvent("close", { code: 1006, reason: "network lost before inflation deadline" });
        raw.fireClose(natural);
        held.release({ kind: "decoded", json: DECODED_PONG });
        await vi.advanceTimersByTimeAsync(0);
        expect(received).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ data: DECODED_PONG }));
        expect(messageListener).toHaveBeenCalledOnce();
        expect(held.fake.decode).toHaveBeenCalledOnce();
        // The ordered gzip close is still behind B. The five-second allowance
        // nevertheless belongs to raw death, not to a future inflation result.
        await vi.advanceTimersByTimeAsync(orderedDrainBoundMs - 1);
        expect(closed).not.toHaveBeenCalled();
        expect(held.fake.close).not.toHaveBeenCalled();
        await vi.advanceTimersByTimeAsync(1);
        expectOriginalGzipClose(wire, natural);

        if (late === "success") {
          inflation.release();
        } else {
          inflation.reject(new Error("B inflater failed after the natural-close deadline"));
        }
        await inflation.finished;
        await vi.advanceTimersByTimeAsync(0);
        // A failed inflater must not close the already-dead raw transport again.
        expectOriginalGzipClose(wire, natural);
        raw.deliverMessage(wire.compressedB.buffer);
        raw.fireError();
        raw.fireClose(natural);
        socket.close();
        await vi.advanceTimersByTimeAsync(orderedDrainBoundMs);
        expectOriginalGzipClose(wire, natural);
        expect(held.fake.decode).toHaveBeenCalledOnce();
        expect(received).toHaveBeenCalledOnce();
        expect(messageListener).toHaveBeenCalledOnce();
      } finally {
        await wire.dispose();
      }
    },
  );

  it.each(["local close", "fatal decoder rejection"] as const)(
    "discards queued real gzip C behind held B on %s before raw death",
    async (terminal) => {
      const wire = await pendingGzipAttachment();
      const { held, raw, socket, received, closed, closeListener, inflation, messageListener } = wire;
      try {
        raw.deliverMessage(wire.compressedB.buffer);
        await inflation.entered;
        raw.deliverMessage(wire.compressedC.buffer);
        expect(inflation.started).toHaveBeenCalledExactlyOnceWith("gzip");
        vi.useFakeTimers();
        if (terminal === "local close") {
          socket.close();
        } else {
          held.reject(new Error("A decoder rejected on a live transport"));
        }
        await vi.advanceTimersByTimeAsync(0);
        // Retirement cannot wait for B's native inflater output to settle.
        expect(closed).toHaveBeenCalledOnce();
        expect(closeListener).toHaveBeenCalledExactlyOnceWith(closed.mock.calls[0][0]);
        expect(held.fake.close).toHaveBeenCalledOnce();
        expect(raw.close).toHaveBeenCalledOnce();
        expect(socket.ws.readyState).toBe(WebSocket.CLOSED);
        if (terminal === "local close") {
          expect(wire.notices).toEqual([]);
          expect(wire.attachment.deliveryPolicy).toEqual({ kind: "negotiate" });
          held.release({ kind: "decoded", json: DECODED_PONG });
        } else {
          expect(wire.notices).toEqual([{
            kind: "failed", reason: "decodeFailure", joinCredentials: GAME_STARTED_CREDENTIALS,
          }]);
          expect(wire.attachment.deliveryPolicy).toEqual({ kind: "snapshotOnly", reason: "decodeFailure" });
        }

        inflation.release();
        await inflation.finished;
        await vi.advanceTimersByTimeAsync(0);
        expect(inflation.started).toHaveBeenCalledExactlyOnceWith("gzip");
        expect(held.fake.decode).toHaveBeenCalledOnce();
        expect(received).not.toHaveBeenCalled();
        expect(messageListener).not.toHaveBeenCalled();
        socket.close();
        raw.fireError();
        await vi.advanceTimersByTimeAsync(orderedDrainBoundMs);
        expect(closed).toHaveBeenCalledOnce();
        expect(closeListener).toHaveBeenCalledOnce();
        expect(held.fake.close).toHaveBeenCalledOnce();
        expect(raw.close).toHaveBeenCalledOnce();
        expect(wire.errored).not.toHaveBeenCalled();
      } finally {
        await wire.dispose();
      }
    },
  );

  it.each(["success", "failure"] as const)(
    "retires a plain gzip socket's queued C and fences active B's late %s on standard close",
    async (late) => {
      const compressedB = await encodeJsonEnvelope('{"type":"Pong","data":{"timestamp":41}}'.padEnd(WIRE_COMPRESSION_THRESHOLD + 1, " "));
      const compressedC = await encodeJsonEnvelope('{"type":"Pong","data":{"timestamp":42}}'.padEnd(WIRE_COMPRESSION_THRESHOLD + 1, " "));
      expect(compressedB[0]).toBe(0x01);
      expect(compressedC[0]).toBe(0x01);
      const pending = openPhaseSocket("ws://test");
      const raw = MockWebSocket.instances[0];
      raw.deliverMessage(helloFrame({ wire_formats: ["GzipEnvelopeV1"] }));
      const socket = await pending;
      const received = vi.fn();
      const errored = vi.fn();
      const closed = vi.fn();
      const closeListener = vi.fn();
      socket.ws.onmessage = received;
      socket.ws.onerror = errored;
      socket.ws.onclose = closed;
      socket.ws.addEventListener("close", closeListener);
      const inflation = heldGzipInflation();
      try {
        raw.deliverMessage(compressedB.buffer);
        await inflation.entered;
        raw.deliverMessage(compressedC.buffer);
        expect(inflation.started).toHaveBeenCalledExactlyOnceWith("gzip");
        vi.useFakeTimers();
        socket.ws.close();
        await vi.advanceTimersByTimeAsync(0);
        expect(raw.close).toHaveBeenCalledOnce();
        expect(socket.ws.readyState).toBe(WebSocket.CLOSED);

        if (late === "success") inflation.release();
        else inflation.reject(new Error("B inflater failed after local close"));
        await inflation.finished;
        await vi.advanceTimersByTimeAsync(0);
        expect(inflation.started).toHaveBeenCalledExactlyOnceWith("gzip");
        expect(received).not.toHaveBeenCalled();
        expect(errored).not.toHaveBeenCalled();
        expect(closed).toHaveBeenCalledOnce();
        expect(closeListener).toHaveBeenCalledExactlyOnceWith(closed.mock.calls[0][0]);
        socket.ws.close();
        raw.deliverMessage(compressedC.buffer);
        raw.fireError();
        await vi.advanceTimersByTimeAsync(0);
        expect(raw.close).toHaveBeenCalledOnce();
        expect(inflation.started).toHaveBeenCalledExactlyOnceWith("gzip");
        expect(received).not.toHaveBeenCalled();
        expect(errored).not.toHaveBeenCalled();
        expect(closed).toHaveBeenCalledOnce();
        expect(closeListener).toHaveBeenCalledOnce();
      } finally {
        inflation.release();
        await inflation.finished;
        inflation.restore();
        vi.useRealTimers();
      }
    },
  );

  it.each(["raw", "gzip"] as const)(
    "preserves the original %s network close when the held decode rejects, without degrading or resyncing",
    async (wire) => {
      const { held, attachment, notices, raw, socket, received, closed, closeListener } =
        await heldAttachment(wire);
      vi.useFakeTimers();
      try {
        const natural = new CloseEvent("close", { code: 1006, reason: "network lost", wasClean: false });
        raw.fireClose(natural);
        await vi.advanceTimersByTimeAsync(0);
        // A finite drain still gives a healthy in-flight frame its chance.
        expect(closed).not.toHaveBeenCalled();
        held.reject(new Error("decoder RPC rejected after network loss"));
        await vi.advanceTimersByTimeAsync(0);
        expect(notices).toEqual([]);
        expect(attachment.deliveryPolicy).toEqual({ kind: "negotiate" });
        expect(closed).toHaveBeenCalledExactlyOnceWith(natural);
        expect(closeListener).toHaveBeenCalledExactlyOnceWith(natural);
        expect(held.fake.close).toHaveBeenCalledOnce();
        expect(received).not.toHaveBeenCalled();
        expect(raw.send).not.toHaveBeenCalledWith('{"type":"ResyncState"}');
        // The network already closed: do not manufacture another raw close.
        expect(raw.close).not.toHaveBeenCalled();
        socket.close();
        raw.fireClose(natural);
        await vi.advanceTimersByTimeAsync(orderedDrainBoundMs);
        expect(closed).toHaveBeenCalledOnce();
        expect(closeListener).toHaveBeenCalledOnce();
        expect(held.fake.close).toHaveBeenCalledOnce();
        expect(notices).toEqual([]);
      } finally {
        vi.useRealTimers();
      }
    },
  );

  it.each(["raw", "gzip"] as const)(
    "does not start recovery when the held %s decode requests resync after network death",
    async (wire) => {
      const { held, attachment, notices, raw, received, closed } = await heldAttachment(wire);
      vi.useFakeTimers();
      try {
        const natural = new CloseEvent("close", { code: 1006, reason: "network lost" });
        raw.fireClose(natural);
        await vi.advanceTimersByTimeAsync(0);
        held.release({ kind: "resyncRequired" });
        await vi.advanceTimersByTimeAsync(0);
        expect(notices).toEqual([]);
        expect(raw.send).not.toHaveBeenCalledWith('{"type":"ResyncState"}');
        expect(attachment.deliveryPolicy).toEqual({ kind: "negotiate" });
        expect(closed).toHaveBeenCalledExactlyOnceWith(natural);
        expect(held.fake.close).toHaveBeenCalledOnce();
        expect(received).not.toHaveBeenCalled();
        expect(raw.close).not.toHaveBeenCalled();
      } finally {
        vi.useRealTimers();
      }
    },
  );

  it.each([
    ["raw", "decoded"], ["gzip", "decoded"],
    ["raw", "rejected"], ["gzip", "rejected"],
  ] as const)(
    "bounds a never-settling %s decode on natural close and fences its late %s outcome",
    async (wire, late) => {
      const { held, attachment, notices, raw, socket, received, closed, closeListener } =
        await heldAttachment(wire);
      vi.useFakeTimers();
      try {
        // More already-received work must not keep the close waiting forever.
        raw.deliverMessage(codecFrame("Delta"));
        raw.deliverMessage('{"type":"Pong","data":{"timestamp":19}}');
        const natural = new CloseEvent("close", { code: 1006, reason: "network lost" });
        raw.fireClose(natural);
        await vi.advanceTimersByTimeAsync(0);
        expect(closed).not.toHaveBeenCalled();
        expect(held.fake.close).not.toHaveBeenCalled();
        // The decoder has no watchdog in this fixture and stays unresolved for
        // the entire drain allowance. Teardown must own its independent bound.
        await vi.advanceTimersByTimeAsync(orderedDrainBoundMs);
        expect(closed).toHaveBeenCalledExactlyOnceWith(natural);
        expect(closeListener).toHaveBeenCalledExactlyOnceWith(natural);
        expect(held.fake.close).toHaveBeenCalledOnce();
        expect(notices).toEqual([]);
        expect(attachment.deliveryPolicy).toEqual({ kind: "negotiate" });
        expect(received).not.toHaveBeenCalled();
        expect(held.fake.decode).toHaveBeenCalledOnce();
        expect(raw.close).not.toHaveBeenCalled();

        if (late === "decoded") {
          held.release({ kind: "decoded", json: DECODED_PONG });
        } else {
          held.reject(new Error("decoder finished after the drain deadline"));
        }
        raw.deliverMessage(codecFrame("Full"));
        raw.fireError();
        raw.fireClose(natural);
        socket.close();
        await vi.advanceTimersByTimeAsync(orderedDrainBoundMs);
        expect(received).not.toHaveBeenCalled();
        expect(held.fake.decode).toHaveBeenCalledOnce();
        expect(closed).toHaveBeenCalledOnce();
        expect(closeListener).toHaveBeenCalledOnce();
        expect(held.fake.close).toHaveBeenCalledOnce();
        expect(notices).toEqual([]);
        expect(raw.send).not.toHaveBeenCalledWith('{"type":"ResyncState"}');
        expect(attachment.deliveryPolicy).toEqual({ kind: "negotiate" });
      } finally {
        vi.useRealTimers();
      }
    },
  );

  it.each(["decoded", "resyncRequired", "rejected"] as const)(
    "reserves network death before a queued frame starts its %s decode",
    async (outcome) => {
      const fake = fakeSession(() => outcome === "resyncRequired"
        ? { kind: "resyncRequired" } : { kind: "decoded", json: DECODED_PONG });
      if (outcome === "rejected") {
        fake.decode.mockImplementationOnce(async () => { throw new Error("queued decode rejected"); });
      }
      const recovery = vi.fn();
      const { raw, socket } = await openCodecSocket(fake, { onRecovery: recovery });
      const received = vi.fn();
      const closed = vi.fn();
      socket.ws.onmessage = received;
      socket.ws.onclose = closed;
      vi.useFakeTimers();
      try {
        raw.deliverMessage(confirmationFrame());
        raw.deliverMessage(codecFrame("Full"));
        const natural = new CloseEvent("close", { code: 1006, reason: "network lost" });
        raw.fireClose(natural);
        // No queue task has started, but transport death is already visible.
        expect(fake.decode).not.toHaveBeenCalled();
        expect(socket.ws.readyState).toBe(MockWebSocket.CLOSED);
        const sent = raw.send.mock.calls.length;
        socket.ws.send('{"type":"Ping"}');
        expect(raw.send).toHaveBeenCalledTimes(sent);
        await vi.advanceTimersByTimeAsync(0);
        expect(fake.decode).toHaveBeenCalledOnce();
        expect(received).toHaveBeenCalledTimes(outcome === "decoded" ? 1 : 0);
        if (outcome === "decoded") {
          expect(received).toHaveBeenCalledWith(expect.objectContaining({ data: DECODED_PONG }));
        }
        expect(closed).toHaveBeenCalledExactlyOnceWith(natural);
        expect(recovery).not.toHaveBeenCalled();
        expect(fake.close).toHaveBeenCalledOnce();
        expect(raw.close).not.toHaveBeenCalled();
        expect(raw.send).not.toHaveBeenCalledWith('{"type":"ResyncState"}');
      } finally {
        vi.useRealTimers();
      }
    },
  );

  it("bounds natural close while waiting for a message consumer and fences a consumer attached afterwards", async () => {
    const fake = fakeSession();
    const recovery = vi.fn();
    const { raw, socket } = await openCodecSocket(fake, { onRecovery: recovery });
    const closed = vi.fn();
    socket.ws.onclose = closed;
    vi.useFakeTimers();
    try {
      raw.deliverMessage(confirmationFrame());
      raw.deliverMessage(codecFrame("Full"));
      await vi.advanceTimersByTimeAsync(0);
      expect(fake.decode).not.toHaveBeenCalled();
      const natural = new CloseEvent("close", { code: 1006, reason: "network lost" });
      raw.fireClose(natural);
      await vi.advanceTimersByTimeAsync(orderedDrainBoundMs);
      expect(closed).toHaveBeenCalledExactlyOnceWith(natural);
      expect(fake.close).toHaveBeenCalledOnce();
      const received = vi.fn();
      socket.ws.onmessage = received;
      raw.deliverMessage(codecFrame("Full"));
      await vi.advanceTimersByTimeAsync(0);
      expect(fake.decode).not.toHaveBeenCalled();
      expect(received).not.toHaveBeenCalled();
      expect(recovery).not.toHaveBeenCalled();
      expect(raw.close).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("drains healthy frames and the queued error when a consumer attaches after network death within the bound", async () => {
    const fake = fakeSession();
    const trace: string[] = [];
    fake.close.mockImplementation(() => { trace.push("lease"); });
    const { raw, socket } = await openCodecSocket(fake);
    socket.ws.onerror = () => { trace.push("error"); };
    socket.ws.onclose = () => { trace.push("close"); };
    vi.useFakeTimers();
    try {
      const plain = '{"type":"Pong","data":{"timestamp":31}}';
      raw.deliverMessage(confirmationFrame());
      raw.deliverMessage(codecFrame("Full"));
      raw.deliverMessage(plain);
      raw.fireError();
      await vi.advanceTimersByTimeAsync(0);
      expect(fake.decode).not.toHaveBeenCalled();
      raw.fireClose(new CloseEvent("close", { code: 1006 }));
      await vi.advanceTimersByTimeAsync(100);
      expect(trace).toEqual([]);
      socket.ws.onmessage = (event) => { trace.push(`message:${event.data}`); };
      await vi.advanceTimersByTimeAsync(0);
      expect(trace).toEqual([`message:${DECODED_PONG}`, `message:${plain}`, "error", "lease", "close"]);
      expect(fake.decode).toHaveBeenCalledOnce();
      expect(fake.close).toHaveBeenCalledOnce();
      await vi.advanceTimersByTimeAsync(orderedDrainBoundMs);
      expect(trace.filter((entry) => entry === "close")).toHaveLength(1);
      expect(fake.close).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it("cancels the existing recovery deadline at network death while draining a healthy recovery Full without publishing ready", async () => {
    let completeFull!: (result: StateCodecDecodeResult) => void;
    const fake = fakeSession();
    fake.decode.mockImplementationOnce(async () => ({ kind: "resyncRequired" }));
    fake.decode.mockImplementationOnce(() => new Promise<StateCodecDecodeResult>((resolve) => {
      completeFull = resolve;
    }));
    const notices: StateCodecRecoveryNotice[] = [];
    const { raw, socket } = await openCodecSocket(fake, { onRecovery: (state) => notices.push(state) });
    const received = vi.fn(() => true);
    const closed = vi.fn();
    socket.ws.onmessage = received;
    socket.ws.onclose = closed;
    vi.useFakeTimers();
    try {
      raw.deliverMessage(confirmationFrame());
      raw.deliverMessage(codecFrame("Delta"));
      await vi.advanceTimersByTimeAsync(0);
      expect(notices).toEqual(["recovering"]);
      raw.deliverMessage(codecFrame("Full"));
      await vi.advanceTimersByTimeAsync(0);
      expect(fake.decode).toHaveBeenCalledTimes(2);
      await vi.advanceTimersByTimeAsync(STATE_CODEC_SOCKET_TIMEOUT_MS - 100);
      const natural = new CloseEvent("close", { code: 1006, reason: "network lost" });
      raw.fireClose(natural);
      await vi.advanceTimersByTimeAsync(100);
      // The former recovery deadline cannot cut short a natural healthy drain.
      expect(closed).not.toHaveBeenCalled();
      expect(notices).toEqual(["recovering"]);
      completeFull({ kind: "decoded", json: DECODED_PONG });
      await vi.advanceTimersByTimeAsync(0);
      expect(received).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ data: DECODED_PONG }));
      expect(closed).toHaveBeenCalledExactlyOnceWith(natural);
      expect(notices).toEqual(["recovering"]);
      expect(fake.close).toHaveBeenCalledOnce();
      expect(raw.close).not.toHaveBeenCalled();
      expect(raw.send.mock.calls.filter(([text]) => text === '{"type":"ResyncState"}')).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("openPhaseSocket — ordered codec errors", () => {
  it("delivers a raw error after the frame the wrapper already received and before the natural close, and fails nothing", async () => {
    const trace: string[] = [];
    const held = heldDecoder();
    held.fake.close.mockImplementation(() => { trace.push("lease"); });
    const attachment = new StateCodecAttachment();
    const binding = attachment.begin();
    const notices: StateCodecRecoveryNotice[] = [];
    let adopted: PhaseSocketTransport | null = null;
    const { raw, socket } = await openCodecSocket(held.fake, {
      bootstrap: () => "pending",
      onRecovery: (state) => {
        notices.push(state);
        attachment.handleRecovery(binding, state, adopted, false, vi.fn());
      },
    });
    adopted = socket.ws;
    attachment.adopt(binding, socket.ws);
    socket.ws.onmessage = () => { trace.push("message"); };
    socket.ws.onerror = () => { trace.push("error"); };
    socket.ws.onclose = () => { trace.push("close"); };
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(gameStartedFullFrame());
    // Reach guard: the frame is inside the decoder, so it has been received.
    await vi.waitFor(() => expect(held.fake.decode).toHaveBeenCalledOnce());

    raw.fireError();
    raw.close();
    await drainMacrotask();
    // Both are queued behind the held frame; neither overtook it.
    expect(trace).toEqual([]);

    held.release({ kind: "decoded", json: DECODED_PONG });
    await vi.waitFor(() => expect(trace).toEqual(["message", "error", "lease", "close"]));
    // An ordinary error is not a codec failure: no notice, no degradation, and
    // the lease is released only by the natural close that follows it.
    expect(notices).toEqual([]);
    expect(attachment.deliveryPolicy).toEqual({ kind: "negotiate" });
    expect(held.fake.close).toHaveBeenCalledOnce();
  });

  it("delivers an error that precedes every frame without a message consumer, ahead of the close, even when the handler throws", async () => {
    const fake = fakeSession();
    const trace: string[] = [];
    const { raw, socket } = await openCodecSocket(fake);
    socket.ws.onerror = () => {
      trace.push("error");
      throw new Error("error handler");
    };
    socket.ws.onclose = () => { trace.push("close"); };
    // Deliberately no onmessage consumer: the queue must not wait for one.
    raw.fireError();
    raw.close();
    await vi.waitFor(() => expect(trace).toEqual(["error", "close"]));
    expect(fake.decode).not.toHaveBeenCalled();
  });

  it("reads the current error handler at delivery: a replacement wins, null drops, a later install gets no replay", async () => {
    const fake = fakeSession();
    const { raw, socket } = await openCodecSocket(fake);
    const first = vi.fn();
    const second = vi.fn();
    socket.ws.onerror = first;
    raw.fireError();
    socket.ws.onerror = second;
    await drainMacrotask();
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledOnce();

    raw.fireError();
    socket.ws.onerror = null;
    await drainMacrotask();
    expect(second).toHaveBeenCalledOnce();

    raw.fireError();
    await drainMacrotask();
    const late = vi.fn();
    socket.ws.onerror = late;
    await drainMacrotask();
    expect(late).not.toHaveBeenCalled();
    // Reach guard: the late handler is live for an error that arrives afterwards.
    raw.fireError();
    await vi.waitFor(() => expect(late).toHaveBeenCalledOnce());
  });

  it("suppresses a queued error and a held frame after an explicit close, releasing the lease once", async () => {
    const held = heldDecoder();
    const trace: string[] = [];
    const { raw, socket } = await openCodecSocket(held.fake);
    socket.ws.onmessage = () => { trace.push("message"); };
    socket.ws.onerror = () => { trace.push("error"); };
    socket.ws.onclose = () => { trace.push("close"); };
    raw.deliverMessage(confirmationFrame());
    // Live counterpart: an error and a plain frame reach their handlers.
    raw.fireError();
    raw.deliverMessage(JSON.stringify({ type: "Pong", data: { timestamp: 1 } }));
    await vi.waitFor(() => expect(trace).toEqual(["error", "message"]));
    raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(held.fake.decode).toHaveBeenCalledOnce());

    raw.fireError();
    socket.close();
    // Invalidation and release are immediate, not queued behind the held decode.
    expect(held.fake.close).toHaveBeenCalledOnce();
    held.release({ kind: "decoded", json: DECODED_PONG });
    await drainMacrotask();
    expect(trace).toEqual(["error", "message", "close"]);
    expect(held.fake.close).toHaveBeenCalledOnce();
  });

  it("delivers a retained fatal close once to a close-only consumer, and suppresses later errors", async () => {
    const fake = fakeSession();
    const { raw, socket } = await openCodecSocket(fake);
    // A codec frame ahead of the confirmation is fatal and needs no consumer.
    raw.deliverMessage(codecFrame("Full"));
    // Reach guard: the fatal close happened with no consumer attached at all.
    await vi.waitFor(() => expect(fake.close).toHaveBeenCalledOnce());

    const closed = vi.fn();
    socket.ws.onclose = closed;
    expect(closed).toHaveBeenCalledOnce();
    const errored = vi.fn();
    socket.ws.onerror = errored;
    raw.fireError();
    raw.close();
    await drainMacrotask();
    expect(errored).not.toHaveBeenCalled();
    expect(closed).toHaveBeenCalledOnce();
  });

  it("keeps the decoded frame ahead of a raw error injected while the codec decoder holds it, through gzip and codec", async () => {
    const held = heldDecoder();
    const trace: string[] = [];
    const { raw, socket } = await openCodecSocket(held.fake, { gzip: true, bootstrap: () => "pending" });
    // Reach guard: both layers were negotiated for this socket.
    expect(raw.send).toHaveBeenCalledWith(expect.stringContaining('"wire_formats":["GzipEnvelopeV1"]'));
    socket.ws.onmessage = () => { trace.push("message"); };
    socket.ws.onerror = () => { trace.push("error"); };
    socket.ws.onclose = () => { trace.push("close"); };
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(gzipFrame(gameStartedFullFrame()));
    // The frame has crossed gzip and is inside the codec decoder.
    await vi.waitFor(() => expect(held.fake.decode).toHaveBeenCalledOnce());
    expect(trace).toEqual([]);

    raw.fireError();
    await drainMacrotask();
    expect(trace).toEqual([]);

    held.release({ kind: "decoded", json: DECODED_PONG });
    await vi.waitFor(() => expect(trace).toEqual(["message", "error"]));
    raw.close();
    await vi.waitFor(() => expect(trace).toEqual(["message", "error", "close"]));
  });
});

describe("readGameStartedFullCredentials", () => {
  it("reads exactly the three credentials, tolerating unknown fields and an invalid codec revision", () => {
    const expected = { ...GAME_STARTED_CREDENTIALS, fullKey: { ...GAME_STARTED_CREDENTIALS.fullKey } };
    expect(readGameStartedFullCredentials(gameStartedFullFrame())).toEqual(expected);
    // Unknown fields at every level are tolerated and never copied.
    const noisy = gameStartedFullFrame(
      { extra: { nested: true }, full_key: { game_code: "GAME01", generation: 3, extra: "x" } },
      { revision: "not-a-revision", more: [1, 2] },
    );
    const read = readGameStartedFullCredentials(noisy);
    expect(read).toEqual(expected);
    expect(Object.keys(read!).sort()).toEqual(["fullKey", "playerToken", "yourPlayer"]);
    expect(Object.keys(read!.fullKey).sort()).toEqual(["game_code", "generation"]);
    // No trimming or normalisation of a present identity.
    expect(readGameStartedFullCredentials(gameStartedFullFrame({ player_token: " tok " }))?.playerToken)
      .toBe(" tok ");
    expect(readGameStartedFullCredentials(gameStartedFullFrame({ your_player: 0 }))?.yourPlayer).toBe(0);
  });

  const withData = (data: unknown) => JSON.stringify({ type: "Full", data });
  const withState = (state: unknown) => withData({ state, revision: 1 });
  const withStarted = (started: unknown) => withState({ type: "GameStarted", data: started });
  const key = { game_code: "GAME01", generation: 3 };
  const valid = { player_token: "join-token", your_player: 1, full_key: key };

  it.each<[string, string]>([
    ["unparseable text", "not json"],
    ["a null document", "null"],
    ["an array document", "[]"],
    ["a Delta frame", JSON.stringify({ type: "Delta", data: JSON.parse(gameStartedFullFrame()).data })],
    ["a StateUpdate frame", JSON.stringify({ type: "StateUpdate", data: JSON.parse(gameStartedFullFrame()).data })],
    ["a missing data object", JSON.stringify({ type: "Full" })],
    ["a null data object", withData(null)],
    ["an array data object", withData([])],
    ["a null state", withState(null)],
    ["an array state", withState([])],
    ["a StateUpdate state", withState({ type: "StateUpdate", data: valid })],
    ["a lower-case GameStarted tag", withState({ type: "gamestarted", data: valid })],
    ["a null GameStarted payload", withStarted(null)],
    ["an array GameStarted payload", withStarted([])],
    ["a missing token", withStarted({ ...valid, player_token: undefined })],
    ["an empty token", withStarted({ ...valid, player_token: "" })],
    ["a numeric token", withStarted({ ...valid, player_token: 7 })],
    ["a missing seat", withStarted({ ...valid, your_player: undefined })],
    ["a negative seat", withStarted({ ...valid, your_player: -1 })],
    ["a fractional seat", withStarted({ ...valid, your_player: 1.5 })],
    ["an unsafe seat", withStarted({ ...valid, your_player: 2 ** 53 })],
    ["a string seat", withStarted({ ...valid, your_player: "1" })],
    ["a null seat", withStarted({ ...valid, your_player: null })],
    ["a missing key", withStarted({ ...valid, full_key: undefined })],
    ["a null key", withStarted({ ...valid, full_key: null })],
    ["an array key", withStarted({ ...valid, full_key: [] })],
    ["a missing game code", withStarted({ ...valid, full_key: { generation: 3 } })],
    ["an empty game code", withStarted({ ...valid, full_key: { game_code: "", generation: 3 } })],
    ["a numeric game code", withStarted({ ...valid, full_key: { game_code: 5, generation: 3 } })],
    ["a missing generation", withStarted({ ...valid, full_key: { game_code: "GAME01" } })],
    ["a zero generation", withStarted({ ...valid, full_key: { game_code: "GAME01", generation: 0 } })],
    ["a negative generation", withStarted({ ...valid, full_key: { game_code: "GAME01", generation: -1 } })],
    ["a fractional generation", withStarted({ ...valid, full_key: { game_code: "GAME01", generation: 1.5 } })],
    ["an unsafe generation", withStarted({ ...valid, full_key: { game_code: "GAME01", generation: 2 ** 53 } })],
    ["a string generation", withStarted({ ...valid, full_key: { game_code: "GAME01", generation: "3" } })],
  ])("reads no credentials from %s", (_label, text) => {
    // Reach guard: the same builders produce a frame the reader accepts.
    expect(readGameStartedFullCredentials(withStarted(valid))).toEqual({
      playerToken: "join-token", yourPlayer: 1, fullKey: key,
    });
    expect(readGameStartedFullCredentials(text)).toBeNull();
  });
});

describe("openPhaseSocket — typed failure credentials", () => {
  const failed = (notices: StateCodecRecoveryNotice[]): StateCodecFailure[] =>
    notices.filter((notice): notice is StateCodecFailure => typeof notice === "object");

  async function openWithNotices(
    fake: ReturnType<typeof fakeSession>,
    bootstrap: () => "pending" | "accepted" = () => "pending",
  ) {
    const notices: StateCodecRecoveryNotice[] = [];
    const { raw, socket } = await openCodecSocket(fake, {
      bootstrap, onRecovery: (state) => { notices.push(state); },
    });
    const closed = vi.fn();
    socket.ws.onclose = closed;
    socket.ws.onmessage = vi.fn();
    return { raw, socket, notices, closed };
  }

  it("carries the credentials of a GameStarted Full rejected by the decoder while the bootstrap is pending", async () => {
    const fake = fakeSession();
    fake.decode.mockImplementationOnce(async () => { throw new Error("decode failed"); });
    const { raw, notices, closed } = await openWithNotices(fake);
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(gameStartedFullFrame());
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(notices).toEqual([{ kind: "failed", reason: "decodeFailure", joinCredentials: GAME_STARTED_CREDENTIALS }]);
  });

  it.each([
    ["a Full that is not a GameStarted", codecFrame("Full")],
    ["a Delta", codecFrame("Delta")],
    ["a GameStarted Full with a damaged seat", gameStartedFullFrame({ your_player: -1 })],
  ])("carries no credentials from a rejected decode of %s", async (_label, frame) => {
    const fake = fakeSession();
    fake.decode.mockImplementationOnce(async () => { throw new Error("decode failed"); });
    const { raw, notices, closed } = await openWithNotices(fake);
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(frame);
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(notices).toEqual([{ kind: "failed", reason: "decodeFailure", joinCredentials: null }]);
  });

  it("carries no credentials once the application bootstrap is accepted", async () => {
    const fake = fakeSession();
    fake.decode.mockImplementationOnce(async () => { throw new Error("decode failed"); });
    const { raw, notices, closed } = await openWithNotices(fake, () => "accepted");
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(gameStartedFullFrame());
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(notices).toEqual([{ kind: "failed", reason: "decodeFailure", joinCredentials: null }]);
  });

  it.each([
    ["the resync is sent", false],
    ["the resync cannot be sent", true],
  ])("carries the rejected GameStarted Full's credentials on a bootstrap rejection when %s", async (_label, sendFails) => {
    let calls = 0;
    const fake = fakeSession(() => ++calls === 1
      ? { kind: "resyncRequired" } : { kind: "decoded", json: DECODED_PONG });
    const { raw, notices, closed } = await openWithNotices(fake);
    raw.deliverMessage(confirmationFrame());
    if (sendFails) raw.send.mockImplementation(() => { throw new Error("send failed"); });
    raw.deliverMessage(gameStartedFullFrame());
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(raw.send).toHaveBeenCalledWith('{"type":"ResyncState"}');
    expect(notices).toEqual([
      "recovering",
      { kind: "failed", reason: "bootstrapRejection", joinCredentials: GAME_STARTED_CREDENTIALS },
    ]);
    // A later Full (the would-be resync response) cannot publish or extract anything.
    raw.deliverMessage(gameStartedFullFrame({ player_token: "other-token" }));
    await drainMacrotask();
    expect(failed(notices)).toHaveLength(1);
    expect(fake.decode).toHaveBeenCalledOnce();
  });

  it("carries no credentials from a rejected non-GameStarted Full on a bootstrap rejection", async () => {
    const fake = fakeSession(() => ({ kind: "resyncRequired" }));
    const { raw, notices, closed } = await openWithNotices(fake);
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(codecFrame("Full"));
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(notices).toEqual([
      "recovering",
      { kind: "failed", reason: "bootstrapRejection", joinCredentials: null },
    ]);
  });

  it("carries no credentials on a recovery Full rejection, whatever that Full holds", async () => {
    const fake = fakeSession(() => ({ kind: "resyncRequired" }));
    const { raw, notices, closed } = await openWithNotices(fake, () => "accepted");
    raw.deliverMessage(confirmationFrame());
    raw.deliverMessage(codecFrame("Delta"));
    await vi.waitFor(() => expect(notices).toEqual(["recovering"]));
    raw.deliverMessage(gameStartedFullFrame());
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(notices).toEqual([
      "recovering",
      { kind: "failed", reason: "recoveryFullRejection", joinCredentials: null },
    ]);
  });

  it("carries the credentials of a GameStarted Full that arrives before the confirmation", async () => {
    const fake = fakeSession();
    const { raw, notices, closed } = await openWithNotices(fake);
    raw.deliverMessage(gameStartedFullFrame());
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(fake.decode).not.toHaveBeenCalled();
    expect(notices).toEqual([
      { kind: "failed", reason: "confirmationFailure", joinCredentials: GAME_STARTED_CREDENTIALS },
    ]);
  });

  it.each([
    ["a Delta ahead of the confirmation", () => codecFrame("Delta")],
    ["a non-GameStarted Full ahead of the confirmation", () => codecFrame("Full")],
    ["a contradictory selection", () => confirmationFrame("OtherCodec")],
    ["a missing selection", () => JSON.stringify({ type: "ServerHello", data: { server_version: "x" } })],
  ])("carries no credentials for %s", async (_label, frame) => {
    const fake = fakeSession();
    const { raw, notices, closed } = await openWithNotices(fake);
    raw.deliverMessage(frame());
    await vi.waitFor(() => expect(closed).toHaveBeenCalledOnce());
    expect(notices).toEqual([{ kind: "failed", reason: "confirmationFailure", joinCredentials: null }]);
  });
});

describe("StateCodecAttachment — first notice and the adoption boundary", () => {
  async function openBound(adoptFirst: boolean) {
    const attachment = new StateCodecAttachment();
    const binding = attachment.begin();
    const delivered: StateCodecRecoveryNotice[] = [];
    const socketAtNotice: (PhaseSocketTransport | null)[] = [];
    let adopted: PhaseSocketTransport | null = null;
    const fake = fakeSession();
    const { raw, socket } = await openCodecSocket(fake, {
      bootstrap: () => "pending",
      onRecovery: (state) => {
        delivered.push(state);
        socketAtNotice.push(binding.socket);
        attachment.handleRecovery(binding, state, adopted, false, vi.fn());
      },
    });
    if (adoptFirst) {
      adopted = socket.ws;
      attachment.adopt(binding, socket.ws);
    }
    const adopt = () => { adopted = socket.ws; attachment.adopt(binding, socket.ws); };
    return { attachment, binding, delivered, socketAtNotice, fake, raw, socket, adopt };
  }

  it("strips the credentials of a notice recorded before adoption, and still consumes its reason on adoption", async () => {
    const run = await openBound(false);
    run.raw.deliverMessage(gameStartedFullFrame());
    await vi.waitFor(() => expect(run.delivered).toHaveLength(1));
    // Reach guard: the wrapper's own notice carried the credentials and was
    // recorded while the binding had no socket; only the recording strips them.
    expect(run.delivered).toEqual([
      { kind: "failed", reason: "confirmationFailure", joinCredentials: GAME_STARTED_CREDENTIALS },
    ]);
    expect(run.socketAtNotice).toEqual([null]);
    expect(run.binding.failure).toEqual({ kind: "failed", reason: "confirmationFailure", joinCredentials: null });
    expect(run.fake.close).toHaveBeenCalledOnce();
    expect(run.fake.decode).not.toHaveBeenCalled();
    expect(run.attachment.deliveryPolicy).toEqual({ kind: "negotiate" });

    run.adopt();
    expect(run.attachment.deliveryPolicy).toEqual({ kind: "snapshotOnly", reason: "confirmationFailure" });
    expect(run.binding.failure?.joinCredentials).toBeNull();
  });

  it("retains the credentials of the same early Full on a notice recorded after adoption, and degrades", async () => {
    const run = await openBound(true);
    run.raw.deliverMessage(gameStartedFullFrame());
    await vi.waitFor(() => expect(run.delivered).toHaveLength(1));
    expect(run.socketAtNotice).toEqual([run.socket.ws]);
    expect(run.binding.failure).toEqual({
      kind: "failed", reason: "confirmationFailure", joinCredentials: GAME_STARTED_CREDENTIALS,
    });
    expect(run.fake.decode).not.toHaveBeenCalled();
    expect(run.fake.close).toHaveBeenCalledOnce();
    expect(run.attachment.deliveryPolicy).toEqual({ kind: "snapshotOnly", reason: "confirmationFailure" });
  });

  it("consumes a never-adopted failure's policy without touching the incumbent's gate or bootstrap", async () => {
    const live = await openBound(true);
    live.attachment.accept(live.socket.ws);
    live.attachment.handleRecovery(live.binding, "recovering", live.socket.ws, false, vi.fn());
    expect(() => live.attachment.assertNotRecovering()).toThrow();
    expect(live.attachment.status(live.binding, live.socket.ws, false)).toBe("accepted");

    const failed = live.attachment.begin();
    failed.failure = { kind: "failed", reason: "decodeFailure", joinCredentials: null };
    live.attachment.consumeFailure(failed);

    expect(live.attachment.deliveryPolicy).toEqual({ kind: "snapshotOnly", reason: "decodeFailure" });
    // The incumbent keeps both its recovery gate and its accepted bootstrap.
    expect(() => live.attachment.assertNotRecovering()).toThrow();
    expect(live.attachment.status(live.binding, live.socket.ws, false)).toBe("accepted");
    expect(live.attachment.status(failed, null, false)).toBe("pending");

    // Reach guard: adopting that same failed binding is what clears the gate
    // and displaces the incumbent, which is why adoption is not used here.
    const control = new StateCodecAttachment();
    const incumbent = control.begin();
    control.adopt(incumbent, live.socket.ws);
    control.accept(live.socket.ws);
    control.handleRecovery(incumbent, "recovering", live.socket.ws, false, vi.fn());
    expect(() => control.assertNotRecovering()).toThrow();
    const candidate = control.begin();
    candidate.failure = { kind: "failed", reason: "decodeFailure", joinCredentials: null };
    control.adopt(candidate, live.socket.ws);
    expect(() => control.assertNotRecovering()).not.toThrow();
    expect(control.status(incumbent, live.socket.ws, false)).toBe("pending");
  });

  it("keeps the complete first notice when a later notice arrives, before and after adoption", async () => {
    const other = { playerToken: "other", yourPlayer: 0, fullKey: { game_code: "OTHER", generation: 9 } } as const;
    const second = { kind: "failed", reason: "decodeFailure", joinCredentials: other } as const;

    const live = await openBound(true);
    live.raw.deliverMessage(gameStartedFullFrame());
    await vi.waitFor(() => expect(live.binding.failure).not.toBeNull());
    const first = live.binding.failure;
    expect(first?.joinCredentials).toEqual(GAME_STARTED_CREDENTIALS);
    live.attachment.handleRecovery(live.binding, second, live.socket.ws, false, vi.fn());
    expect(live.binding.failure).toBe(first);

    MockWebSocket.instances.length = 0;
    const early = await openBound(false);
    early.raw.deliverMessage(gameStartedFullFrame());
    await vi.waitFor(() => expect(early.binding.failure).not.toBeNull());
    const stripped = early.binding.failure;
    early.adopt();
    // A later, credential-bearing notice on the now-adopted binding cannot replace it.
    early.attachment.handleRecovery(early.binding, second, early.socket.ws, false, vi.fn());
    expect(early.binding.failure).toBe(stripped);
    expect(early.binding.failure).toEqual({ kind: "failed", reason: "confirmationFailure", joinCredentials: null });
    expect(early.attachment.deliveryPolicy).toEqual({ kind: "snapshotOnly", reason: "confirmationFailure" });
  });
});

describe("writeSetupFrame", () => {
  const transport = (extra: Partial<PhaseSocketTransport>): PhaseSocketTransport => ({
    readyState: WebSocket.OPEN, send: vi.fn(), close: vi.fn(), ...extra,
  }) as unknown as PhaseSocketTransport;

  it("reports a transport without a receipt through its synchronous send", async () => {
    const socket = transport({});
    const sendSync = vi.fn(() => true);
    await expect(writeSetupFrame(socket, { type: "Reconnect" }, undefined, sendSync)).resolves.toBe(true);
    expect(sendSync).toHaveBeenCalledWith({ type: "Reconnect" }, socket);
    sendSync.mockReturnValue(false);
    await expect(writeSetupFrame(socket, { type: "Reconnect" }, undefined, sendSync)).resolves.toBe(false);
  });

  it("writes nothing for an already-aborted signal or a socket that is not open", async () => {
    const sendReceipt = vi.fn(async () => undefined);
    const sendSync = vi.fn(() => true);
    const controller = new AbortController();
    controller.abort();
    await expect(writeSetupFrame(transport({ sendReceipt }), {}, controller.signal, sendSync)).resolves.toBe(false);
    await expect(writeSetupFrame(
      transport({ sendReceipt, readyState: WebSocket.CLOSING }), {}, undefined, sendSync,
    )).resolves.toBe(false);
    expect(sendReceipt).not.toHaveBeenCalled();
    expect(sendSync).not.toHaveBeenCalled();
  });

  it("reports a rejected receipt as not written", async () => {
    const sendReceipt = vi.fn(async () => { throw new Error("write failed"); });
    await expect(writeSetupFrame(transport({ sendReceipt }), {}, undefined, vi.fn())).resolves.toBe(false);
  });

  it("keeps a resolved receipt written when the signal aborts while it is in flight", async () => {
    const controller = new AbortController();
    const sendReceipt = vi.fn(async () => { controller.abort(); });
    await expect(writeSetupFrame(
      transport({ sendReceipt }), { type: "Reconnect" }, controller.signal, vi.fn(),
    )).resolves.toBe(true);
    expect(sendReceipt).toHaveBeenCalledWith('{"type":"Reconnect"}', controller.signal);
  });
});
