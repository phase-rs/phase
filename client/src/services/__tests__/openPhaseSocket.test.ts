import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  HandshakeError,
  openPhaseSocket,
  withReconnect,
} from "../openPhaseSocket";

class MockWebSocket extends EventTarget {
  static OPEN = 1;
  static instances: MockWebSocket[] = [];
  readyState = MockWebSocket.OPEN;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
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
  deliverMessage(data: string) {
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
  }> = {},
): string {
  return JSON.stringify({
    type: "ServerHello",
    data: {
      server_version: "0.0.0-test",
      build_commit: "testhash",
      protocol_version: 3,
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
  it("resolves with serverInfo once ServerHello arrives and sends ClientHello", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame());

    const socket = await promise;
    expect(socket.serverInfo.mode).toBe("Full");
    expect(socket.serverInfo.protocolVersion).toBe(3);
    expect(ws.send).toHaveBeenCalledWith(
      expect.stringContaining('"type":"ClientHello"'),
    );
  });

  it("rejects with protocol_mismatch when versions diverge and closes the socket", async () => {
    const promise = openPhaseSocket("ws://test");
    const ws = MockWebSocket.instances[0];
    ws.deliverMessage(helloFrame({ protocol_version: 99 }));

    await expect(promise).rejects.toBeInstanceOf(HandshakeError);
    expect(ws.close).toHaveBeenCalled();
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
