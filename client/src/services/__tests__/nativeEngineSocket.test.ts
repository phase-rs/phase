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

    expect(events).toEqual(["error", "close"]);
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

    expect(events).toEqual(["error", "close"]);
    expect(socket.readyState).toBe(NativeEngineSocket.CLOSED);
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

    expect(onceListener).toHaveBeenCalledTimes(1);
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
