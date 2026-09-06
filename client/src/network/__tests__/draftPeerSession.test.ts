import { afterEach, describe, expect, it, vi } from "vitest";

import { createDraftPeerSession } from "../draftPeerSession";
import * as protocol from "../draftProtocol";
import type { DraftP2PMessage } from "../draftProtocol";
import { FakeDraftDataConnection } from "./fakeDraftDataConnection";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

// Let already-released work settle while the test's explicit gate stays shut.
const flushAsync = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

const firstMessage: DraftP2PMessage = { type: "draft_error", reason: "first" };
const secondMessage: DraftP2PMessage = { type: "draft_error", reason: "second" };

function createTestSession() {
  const conn = new FakeDraftDataConnection();
  // The fake implements the DataConnection surface used by this session.
  const session = createDraftPeerSession(conn as never);
  return { conn, session };
}

afterEach(() => vi.restoreAllMocks());

describe("DraftPeerSession send lifecycle", () => {
  it.each(["local close", "error", "not open"] as const)("rejects sends after %s without encoding them", async (end) => {
    const encode = vi.spyOn(protocol, "encodeDraftWireMessage");
    const { conn, session } = createTestSession();
    if (end === "local close") session.close();
    else if (end === "error") conn.simulateError(new Error("connection failed"));
    else conn.open = false;

    await expect(session.send(firstMessage)).rejects.toThrow("Draft connection is not open");
    // Best-effort callers may ignore the returned promise without creating an
    // unhandled rejection; the queue still owns its internal recovery catch.
    void session.send(secondMessage);
    await flushAsync();

    expect(encode).not.toHaveBeenCalled();
    expect(conn.sentRaw).toEqual([]);
    session.close();
  });

  it("rejects sends while accepted incoming messages are still draining", async () => {
    const { conn, session } = createTestSession();
    const started = deferred<void>();
    const finish = deferred<void>();
    const disconnected = vi.fn();
    const observed = vi.fn();
    session.onDisconnect(disconnected);
    session.onMessage(async (message) => {
      started.resolve();
      await finish.promise;
      observed(message);
    });
    const received = conn.receiveRaw(await protocol.encodeDraftWireMessage(firstMessage));
    await started.promise;
    conn.simulateClose();

    try {
      await expect(session.send(secondMessage)).rejects.toThrow("Draft connection is not open");
      expect(disconnected).not.toHaveBeenCalled();
      expect(conn.sentRaw).toEqual([]);
    } finally {
      finish.resolve();
      await received;
    }

    expect(observed).toHaveBeenCalledExactlyOnceWith(firstMessage);
    expect(disconnected).toHaveBeenCalledExactlyOnceWith("connection closed");
  });

  it.each(["remote close", "local close", "error", "not open"] as const)("rejects encoding and queued sends when %s interrupts encoding", async (end) => {
    const encoding = deferred<Uint8Array>();
    const started = deferred<void>();
    const encode = vi.spyOn(protocol, "encodeDraftWireMessage").mockImplementationOnce(() => {
      started.resolve();
      return encoding.promise;
    });
    const { conn, session } = createTestSession();
    const send = vi.spyOn(conn, "send");
    const first = session.send(firstMessage);
    await started.promise;
    const second = session.send(secondMessage);
    if (end === "remote close") conn.simulateClose();
    else if (end === "local close") session.close();
    else if (end === "error") conn.simulateError(new Error("connection failed"));
    else conn.open = false;
    encoding.resolve(new Uint8Array([0]));

    const outcomes = await Promise.allSettled([first, second]);

    expect(outcomes).toEqual([
      { status: "rejected", reason: new Error("Draft connection is not open") },
      { status: "rejected", reason: new Error("Draft connection is not open") },
    ]);
    expect(encode).toHaveBeenCalledTimes(1);
    expect(send).not.toHaveBeenCalled();
    session.close();
  });

  it.each(["encoding", "channel"] as const)("does not poison later sends after an %s failure", async (failure) => {
    const { conn, session } = createTestSession();
    const error = new Error("send failed");
    if (failure === "encoding") {
      vi.spyOn(protocol, "encodeDraftWireMessage").mockRejectedValueOnce(error);
    } else {
      vi.spyOn(conn, "send").mockImplementationOnce(() => { throw error; });
    }

    await expect(session.send(firstMessage)).rejects.toBe(error);
    await session.send(secondMessage);

    expect(conn.sentRaw).toHaveLength(1);
    await expect(protocol.decodeDraftWireMessage(conn.sentRaw[0]!)).resolves.toEqual(secondMessage);
    session.close();
  });
});

describe("DraftPeerSession receive ordering", () => {
  it("preserves arrival order when the first decode takes longer", async () => {
    const firstDecode = deferred<DraftP2PMessage>();
    const decodeStarted = deferred<void>();
    const decode = vi.spyOn(protocol, "decodeDraftWireMessage")
      .mockImplementationOnce(() => {
        decodeStarted.resolve();
        return firstDecode.promise;
      })
      .mockResolvedValueOnce(secondMessage);
    const { conn, session } = createTestSession();
    const received: DraftP2PMessage[] = [];
    session.onMessage((msg) => { received.push(msg); });

    const first = conn.receiveRaw(new Uint8Array([1]));
    await decodeStarted.promise;
    const second = conn.receiveRaw(new Uint8Array([2]));
    await flushAsync();
    firstDecode.resolve(firstMessage);
    await Promise.all([first, second]);
    await vi.waitFor(() => expect(received).toHaveLength(2));

    expect(decode).toHaveBeenCalledTimes(2);
    expect(received).toEqual([firstMessage, secondMessage]);
    session.close();
  });

  it("awaits every async handler before dispatching the next message", async () => {
    vi.spyOn(protocol, "decodeDraftWireMessage")
      .mockResolvedValueOnce(firstMessage)
      .mockResolvedValueOnce(secondMessage);
    const handlerStarted = deferred<void>();
    const finishHandler = deferred<void>();
    const { conn, session } = createTestSession();
    const order: string[] = [];
    session.onMessage(async (msg) => {
      if (msg === firstMessage) {
        handlerStarted.resolve();
        await finishHandler.promise;
      }
      order.push(msg === firstMessage ? "first handled" : "second handled");
    });
    session.onMessage((msg) => {
      order.push(msg === firstMessage ? "first observed" : "second observed");
    });

    const first = conn.receiveRaw(new Uint8Array([1]));
    await handlerStarted.promise;
    const second = conn.receiveRaw(new Uint8Array([2]));
    await flushAsync();
    finishHandler.resolve();
    await Promise.all([first, second]);
    await vi.waitFor(() => expect(order).toHaveLength(4));

    expect(order).toEqual([
      "first handled", "first observed", "second handled", "second observed",
    ]);
    session.close();
  });

  it.each(["throw", "reject"] as const)("isolates a handler %s from other handlers and later messages", async (failure) => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.spyOn(protocol, "decodeDraftWireMessage")
      .mockResolvedValueOnce(firstMessage)
      .mockResolvedValueOnce(secondMessage);
    const { conn, session } = createTestSession();
    const error = new Error("handler failed");
    session.onMessage((msg) => {
      if (msg !== firstMessage) return;
      if (failure === "throw") throw error;
      return Promise.reject(error);
    });
    const observed = vi.fn();
    session.onMessage(observed);

    await conn.receiveRaw(new Uint8Array([1]));
    await conn.receiveRaw(new Uint8Array([2]));
    await flushAsync();

    expect(observed.mock.calls.map(([msg]) => msg)).toEqual([firstMessage, secondMessage]);
    expect(warn).toHaveBeenCalledWith("[DraftPeerSession] message handler threw:", error, "draft_error");
    session.close();
  });

  it("continues after malformed binary and non-binary input", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { conn, session } = createTestSession();
    const observed = vi.fn();
    session.onMessage(observed);

    await conn.receiveRaw("not binary");
    await conn.receiveRaw(new Uint8Array([0xff]));
    await conn.receiveRaw(await protocol.encodeDraftWireMessage(secondMessage));
    await vi.waitFor(() => expect(observed).toHaveBeenCalledOnce());

    expect(observed).toHaveBeenCalledWith(secondMessage);
    expect(warn).toHaveBeenCalledWith("[DraftPeerSession] decode error:", expect.any(Error));
    session.close();
  });

  it("decodes gzip Uint8Array and raw ArrayBuffer messages in order", async () => {
    const compressedMessage: DraftP2PMessage = { type: "draft_error", reason: "x".repeat(300) };
    const compressed = await protocol.encodeDraftWireMessage(compressedMessage);
    const raw = await protocol.encodeDraftWireMessage(secondMessage);
    expect(compressed[0]).toBe(0x01);
    expect(raw[0]).toBe(0x00);
    const { conn, session } = createTestSession();
    const received: DraftP2PMessage[] = [];
    session.onMessage((msg) => { received.push(msg); });

    await Promise.all([conn.receiveRaw(compressed), conn.receiveRaw(new Uint8Array(raw).buffer)]);
    await vi.waitFor(() => expect(received).toHaveLength(2));

    expect(received).toEqual([compressedMessage, secondMessage]);
    session.close();
  });

  it.each(["local close", "error"] as const)("drops pending decodes and queued messages after %s", async (end) => {
    const firstDecode = deferred<DraftP2PMessage>();
    const decodeStarted = deferred<void>();
    const decode = vi.spyOn(protocol, "decodeDraftWireMessage")
      .mockImplementation(() => {
        decodeStarted.resolve();
        return firstDecode.promise;
      });
    const { conn, session } = createTestSession();
    const observed = vi.fn();
    session.onMessage(observed);

    const first = conn.receiveRaw(new Uint8Array([1]));
    await decodeStarted.promise;
    const second = conn.receiveRaw(new Uint8Array([2]));
    if (end === "local close") session.close();
    else conn.simulateError(new Error("connection failed"));
    // Even a late subscription must not receive a pre-disconnect decode.
    session.onMessage(observed);
    firstDecode.resolve(firstMessage);
    await Promise.all([first, second]);
    await conn.receiveRaw(new Uint8Array([3]));
    await flushAsync();

    expect(decode).toHaveBeenCalledTimes(1);
    expect(observed).not.toHaveBeenCalled();
  });

  it.each(["decode", "handler"] as const)("drains accepted messages before remote close during an async %s", async (delay) => {
    const started = deferred<void>();
    const finish = deferred<void>();
    const decode = vi.spyOn(protocol, "decodeDraftWireMessage")
      .mockImplementationOnce(async () => {
        if (delay === "decode") {
          started.resolve();
          await finish.promise;
        }
        return firstMessage;
      })
      .mockResolvedValueOnce(secondMessage);
    const conn = new FakeDraftDataConnection();
    const onSessionEnd = vi.fn();
    const session = createDraftPeerSession(conn as never, { onSessionEnd });
    const order: string[] = [];
    session.onMessage(async (msg) => {
      if (delay === "handler" && msg === firstMessage) {
        started.resolve();
        await finish.promise;
      }
      order.push(msg === firstMessage ? "first" : "second");
    });
    const disconnected = vi.fn(() => { order.push("disconnected"); });
    session.onDisconnect(disconnected);

    const first = conn.receiveRaw(new Uint8Array([1]));
    await started.promise;
    const second = conn.receiveRaw(new Uint8Array([2]));
    conn.simulateClose();
    await conn.receiveRaw(new Uint8Array([3]));
    expect(disconnected).not.toHaveBeenCalled();
    finish.resolve();
    await Promise.all([first, second]);
    await vi.waitFor(() => expect(disconnected).toHaveBeenCalledOnce());
    session.close();
    conn.simulateError(new Error("late error"));

    expect(order).toEqual(["first", "second", "disconnected"]);
    expect(decode).toHaveBeenCalledTimes(2);
    expect(disconnected).toHaveBeenCalledWith("connection closed");
    expect(onSessionEnd).toHaveBeenCalledOnce();
  });

  it.each(["local close", "error"] as const)("cancels a remote-close drain on %s", async (end) => {
    const started = deferred<void>();
    const finish = deferred<DraftP2PMessage>();
    const decode = vi.spyOn(protocol, "decodeDraftWireMessage")
      .mockImplementation(() => {
        started.resolve();
        return finish.promise;
      });
    const { conn, session } = createTestSession();
    const observed = vi.fn();
    const disconnected = vi.fn();
    session.onMessage(observed);
    session.onDisconnect(disconnected);

    const first = conn.receiveRaw(new Uint8Array([1]));
    await started.promise;
    const second = conn.receiveRaw(new Uint8Array([2]));
    conn.simulateClose();
    if (end === "local close") session.close("retired");
    else conn.simulateError(new Error("connection failed"));
    expect(disconnected).toHaveBeenCalledOnce();
    finish.resolve(firstMessage);
    await Promise.all([first, second]);
    await flushAsync();

    expect(observed).not.toHaveBeenCalled();
    expect(decode).toHaveBeenCalledTimes(1);
    expect(disconnected).toHaveBeenCalledExactlyOnceWith(end === "local close" ? "retired" : "connection failed");
  });
});
