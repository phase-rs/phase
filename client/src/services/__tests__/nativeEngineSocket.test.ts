import { beforeEach, describe, expect, it, vi } from "vitest";

const { ChannelMock, channelConstructMock, emitChannelEvent, invokeMock, isDesktopTauriMock, resetChannelMock } = vi.hoisted(() => {
  let listener: ((event: unknown) => void) | undefined;
  const channelConstructMock = vi.fn();

  class ChannelMock<T> {
    constructor(callback: (event: T) => void) {
      channelConstructMock();
      listener = callback as (event: unknown) => void;
    }
  }

  return {
    ChannelMock,
    channelConstructMock,
    emitChannelEvent(event: unknown) {
      listener?.(event);
    },
    invokeMock: vi.fn(),
    isDesktopTauriMock: vi.fn(),
    resetChannelMock() {
      listener = undefined;
    },
  };
});

vi.mock("@tauri-apps/api/core", () => ({
  Channel: ChannelMock,
  invoke: invokeMock,
}));
vi.mock("../platform", () => ({ isDesktopTauri: isDesktopTauriMock }));

import { NativeEngineSocket } from "../nativeEngineSocket";
import { openPhaseSocket } from "../openPhaseSocket";

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
};

function deferred<T>(): Deferred<T> {
  let resolve: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve: resolve! };
}

async function resolveConnection(connection: Deferred<number>, bridgeId = 7): Promise<void> {
  connection.resolve(bridgeId);
  await connection.promise;
  await vi.waitFor(() => {
    expect(invokeMock).toHaveBeenCalledWith("connect_native_engine", expect.any(Object));
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  resetChannelMock();
  invokeMock.mockResolvedValue(undefined);
  isDesktopTauriMock.mockReturnValue(true);
});

describe("NativeEngineSocket", () => {
  it("does not construct a channel or invoke commands outside desktop Tauri", async () => {
    isDesktopTauriMock.mockReturnValue(false);
    const socket = new NativeEngineSocket();
    const events: string[] = [];
    socket.onerror = () => events.push("error");
    socket.onclose = () => events.push("close");

    expect(socket.readyState).toBe(NativeEngineSocket.CONNECTING);
    await vi.waitFor(() => expect(socket.readyState).toBe(NativeEngineSocket.CLOSED));

    await vi.waitFor(() => expect(events).toEqual(["error", "close"]));
    expect(channelConstructMock).not.toHaveBeenCalled();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("rejects a non-desktop phase handshake instead of hanging", async () => {
    isDesktopTauriMock.mockReturnValue(false);

    await expect(
      openPhaseSocket("ws://native-engine", {
        socketFactory: () => new NativeEngineSocket(),
        timeoutMs: 100,
      }),
    ).rejects.toThrow("WebSocket error during handshake");
    expect(invokeMock).not.toHaveBeenCalled();
  });
  it("buffers Channel messages until connection resolves and preserves their order", async () => {
    const connection = deferred<number>();
    invokeMock.mockImplementation((command: string) => {
      return command === "connect_native_engine" ? connection.promise : Promise.resolve(undefined);
    });
    const socket = new NativeEngineSocket();
    const messages: string[] = [];
    socket.onmessage = (event) => messages.push(event.data);

    expect(socket.readyState).toBe(NativeEngineSocket.CONNECTING);
    expect(socket.readyState).toBe(0);
    await vi.waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("connect_native_engine", expect.any(Object));
    });

    emitChannelEvent({ type: "message", text: "first" });
    emitChannelEvent({ type: "message", text: "second" });

    expect(messages).toEqual([]);

    await resolveConnection(connection);

    expect(socket.readyState).toBe(NativeEngineSocket.OPEN);
    expect(socket.readyState).toBe(1);
    expect(messages).toEqual(["first", "second"]);
  });

  it("finishes closing after a connection that was closed while connecting settles", async () => {
    const connection = deferred<number>();
    invokeMock.mockImplementation((command: string) => {
      return command === "connect_native_engine" ? connection.promise : Promise.resolve(undefined);
    });
    const socket = new NativeEngineSocket();
    const onclose = vi.fn();
    socket.onclose = onclose;

    socket.close();

    expect(socket.readyState).toBe(NativeEngineSocket.CLOSING);

    await resolveConnection(connection, 41);

    await vi.waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("native_engine_bridge_close", { id: 41 });
    });

    emitChannelEvent({ type: "closed", code: 1000, reason: "normal" });

    expect(socket.readyState).toBe(NativeEngineSocket.CLOSED);
    expect(socket.readyState).toBe(3);
    expect(onclose).toHaveBeenCalledTimes(1);
  });

  it("dispatches errors before close exactly once", async () => {
    const connection = deferred<number>();
    invokeMock.mockImplementation((command: string) => {
      return command === "connect_native_engine" ? connection.promise : Promise.resolve(undefined);
    });
    const socket = new NativeEngineSocket();
    const events: string[] = [];
    socket.onerror = () => events.push("error");
    socket.onclose = () => events.push("close");

    await resolveConnection(connection);

    emitChannelEvent({ type: "error", detail: "read failed" });
    emitChannelEvent({ type: "closed", code: 1006, reason: "read failed" });
    emitChannelEvent({ type: "closed", code: 1006, reason: "duplicate" });

    await vi.waitFor(() => expect(events).toEqual(["error", "close"]));
    expect(socket.readyState).toBe(NativeEngineSocket.CLOSED);
  });

  /**
   * `handleBridgeFailure` is only ever reached from a floating promise
   * (`connect`, `send`, `closeBridge`), so a throwing `onerror` escapes as an
   * unhandled rejection: the guard below restores the cleanup, it deliberately
   * does not swallow. Own those rejections for the row rather than let them
   * fail the run as strays. Same shape as `LoopShortcutModal.test.tsx`.
   */
  function captureUnhandled() {
    const seen: unknown[] = [];
    const prior = process.listeners("unhandledRejection");
    process.removeAllListeners("unhandledRejection");
    const onUnhandled = (reason: unknown) => seen.push(reason);
    process.on("unhandledRejection", onUnhandled);
    return {
      sentinels: (tag: string) => seen.filter((r) => r instanceof Error && r.message === tag),
      restore() {
        process.off("unhandledRejection", onUnhandled);
        for (const l of prior) process.on("unhandledRejection", l as never);
      },
    };
  }

  // `withReconnect` recovers a dropped transport from the close event alone. A
  // caller-assigned `onerror` that throws used to skip `finishClose` entirely,
  // leaving the socket CONNECTING forever with nothing to recover from.
  it("closes after a throwing onerror on the platform-boundary failure path", async () => {
    const capture = captureUnhandled();
    try {
      isDesktopTauriMock.mockReturnValue(false);
      const throwing = new NativeEngineSocket();
      const throwingDrop = vi.fn();
      throwing.addEventListener("close", throwingDrop, { once: true });
      throwing.onerror = () => {
        throw new Error("t5-onerror-threw");
      };

      await vi.waitFor(() => expect(throwing.readyState).toBe(NativeEngineSocket.CLOSED));
      expect(throwingDrop).toHaveBeenCalledTimes(1);
      await vi.waitFor(() => expect(capture.sentinels("t5-onerror-threw")).toHaveLength(1));

      // Control: a non-throwing `onerror` reaches the identical outcome, so the
      // assertions above are about the throw and not about this path in general.
      const quiet = new NativeEngineSocket();
      const quietDrop = vi.fn();
      quiet.addEventListener("close", quietDrop, { once: true });
      quiet.onerror = vi.fn();

      await vi.waitFor(() => expect(quiet.readyState).toBe(NativeEngineSocket.CLOSED));
      expect(quietDrop).toHaveBeenCalledTimes(1);
      expect(capture.sentinels("t5-onerror-threw")).toHaveLength(1);
    } finally {
      capture.restore();
    }
  });

  // `withReconnect` registers `onDrop` as a close LISTENER, not as `onclose`,
  // so the listener loop is the cleanup that must survive a throwing `onclose`.
  it("runs close listeners when onclose throws, without swallowing the throw", async () => {
    const connection = deferred<number>();
    invokeMock.mockImplementation((command: string) => {
      return command === "connect_native_engine" ? connection.promise : Promise.resolve(undefined);
    });
    const socket = new NativeEngineSocket();
    await resolveConnection(connection);
    // OPEN before emitting: `handleBridgeEvent` buffers while CONNECTING, and a
    // buffered event is flushed asynchronously from inside `connect()`, where
    // the throw would never reach this call site.
    await vi.waitFor(() => expect(socket.readyState).toBe(NativeEngineSocket.OPEN));
    const onDrop = vi.fn();
    socket.addEventListener("close", onDrop, { once: true });
    socket.onclose = () => {
      throw new Error("t6-onclose-threw");
    };

    // The `finally` restores the loop but does not catch, and there is no
    // promise chain between here and it: the throw travels `finishClose` ->
    // `dispatchBridgeEvent` -> `handleBridgeEvent` -> the `Channel` callback
    // and surfaces at this synchronous call site. The gzip twin swallows the
    // same throw inside its receive queue; this transport has no such queue.
    expect(() => emitChannelEvent({ type: "closed", code: 1006, reason: "bridge died" }))
      .toThrow("t6-onclose-threw");
    expect(onDrop).toHaveBeenCalledTimes(1);
    expect(socket.readyState).toBe(NativeEngineSocket.CLOSED);

    // Control: a non-throwing `onclose` runs its listener and does not throw at
    // the emit site, so the assertions above discriminate the throwing handler.
    invokeMock.mockImplementation((command: string) => {
      return command === "connect_native_engine" ? Promise.resolve(9) : Promise.resolve(undefined);
    });
    const quiet = new NativeEngineSocket();
    await vi.waitFor(() => expect(quiet.readyState).toBe(NativeEngineSocket.OPEN));
    const quietDrop = vi.fn();
    quiet.addEventListener("close", quietDrop, { once: true });
    quiet.onclose = vi.fn();

    expect(() => emitChannelEvent({ type: "closed", code: 1000, reason: "normal" })).not.toThrow();
    expect(quietDrop).toHaveBeenCalledTimes(1);
    expect(quiet.readyState).toBe(NativeEngineSocket.CLOSED);
  });

  it("honors once close listeners and removes listeners before close", async () => {
    const connection = deferred<number>();
    invokeMock.mockImplementation((command: string) => {
      return command === "connect_native_engine" ? connection.promise : Promise.resolve(undefined);
    });
    const socket = new NativeEngineSocket();
    const onceListener = vi.fn();
    const removedListener = vi.fn();

    await resolveConnection(connection);

    socket.addEventListener("close", onceListener, { once: true });
    socket.addEventListener("close", removedListener);
    socket.removeEventListener("close", removedListener);

    emitChannelEvent({ type: "closed", code: 1000, reason: "normal" });
    emitChannelEvent({ type: "closed", code: 1000, reason: "duplicate" });

    await vi.waitFor(() => expect(onceListener).toHaveBeenCalledTimes(1));
    expect(removedListener).not.toHaveBeenCalled();
    expect(
      (
        socket as unknown as {
          closeListeners: Map<(event: CloseEvent) => void, boolean>;
        }
      ).closeListeners.has(onceListener),
    ).toBe(false);
  });
});

it("routes LAN lifecycle and a close-before-connect race to the independent bridge", async () => {
  const connection = deferred<number>();
  invokeMock.mockImplementation((command: string) => command === "connect_lan_server" ? connection.promise : Promise.resolve());
  const socket = new NativeEngineSocket({ type: "lan", url: "ws://192.168.1.2:9374/ws", origin: "https://phase-rs.dev" });
  socket.close();
  await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("connect_lan_server", {
    url: "ws://192.168.1.2:9374/ws", origin: "https://phase-rs.dev", onEvent: expect.any(ChannelMock),
  }));
  connection.resolve(42);
  await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("lan_bridge_close", { id: 42 }));
  emitChannelEvent({ type: "closed", code: 1000, reason: "" });
  expect(socket.readyState).toBe(NativeEngineSocket.CLOSED);
  expect(invokeMock).not.toHaveBeenCalledWith("native_engine_bridge_close", expect.anything());
});

it("sends LAN text frames and delivers buffered hello without a binary surface", async () => {
  invokeMock.mockResolvedValue(42);
  const socket = new NativeEngineSocket({ type: "lan", url: "ws://10.0.0.2:9374/ws", origin: "https://phase-rs.dev" });
  await vi.waitFor(() => expect(socket.readyState).toBe(NativeEngineSocket.OPEN));
  socket.send("hello");
  await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("lan_bridge_send", { id: 42, text: "hello" }));
  expect("binaryType" in socket).toBe(false);
  socket.close();
});


it("restores a stored LAN URL's default port in the native payload", async () => {
  invokeMock.mockResolvedValue(42);
  const socket = new NativeEngineSocket({ type: "lan", url: "ws://192.168.1.2/ws", origin: "https://phase-rs.dev" });
  await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("connect_lan_server", {
    url: "ws://192.168.1.2:80/ws", origin: "https://phase-rs.dev", onEvent: expect.any(ChannelMock),
  }));
  socket.close();
});
