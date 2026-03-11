import { describe, it, expect, vi } from "vitest";

import { createPeerSession } from "../peer";
import { validateMessage } from "../protocol";

type DataHandler = (data: unknown) => void;
type VoidHandler = () => void;
type ErrorHandler = (err: Error) => void;

/** Minimal fake matching PeerJS DataConnection API surface used by createPeerSession */
class FakeDataConnection {
  open = true;
  sent: unknown[] = [];

  private dataHandlers = new Set<DataHandler>();
  private closeHandlers = new Set<VoidHandler>();
  private errorHandlers = new Set<ErrorHandler>();

  send(data: unknown) {
    if (!this.open) throw new Error("Connection is closed");
    this.sent.push(data);
  }

  on(event: string, handler: (...args: unknown[]) => void): this {
    if (event === "data") this.dataHandlers.add(handler as DataHandler);
    else if (event === "close") this.closeHandlers.add(handler as VoidHandler);
    else if (event === "error") this.errorHandlers.add(handler as ErrorHandler);
    return this;
  }

  off(event: string, handler: (...args: unknown[]) => void): this {
    if (event === "data") this.dataHandlers.delete(handler as DataHandler);
    else if (event === "close") this.closeHandlers.delete(handler as VoidHandler);
    else if (event === "error") this.errorHandlers.delete(handler as ErrorHandler);
    return this;
  }

  // Test helpers
  simulateData(data: unknown) {
    for (const h of this.dataHandlers) h(data);
  }

  simulateClose() {
    this.open = false;
    for (const h of this.closeHandlers) h();
  }
}

function createTestSession() {
  const conn = new FakeDataConnection();
  const destroyPeer = vi.fn();
  // Cast to satisfy DataConnection type -- we only use the subset FakeDataConnection implements
  const session = createPeerSession(conn as never, destroyPeer);
  return { conn, destroyPeer, session };
}

describe("P2P Protocol - validateMessage", () => {
  it("accepts valid P2P message types", () => {
    const msg = { type: "action", action: { type: "PassPriority" } };
    expect(validateMessage(msg)).toEqual(msg);

    const concede = { type: "concede" };
    expect(validateMessage(concede)).toEqual(concede);

    const ping = { type: "ping", timestamp: 12345 };
    expect(validateMessage(ping)).toEqual(ping);
  });

  it("rejects unknown message types", () => {
    expect(() => validateMessage({ type: "unknown_garbage" })).toThrow("Invalid message type");
  });

  it("rejects missing type field", () => {
    expect(() => validateMessage({})).toThrow("Invalid message: missing type field");
    expect(() => validateMessage(null)).toThrow("Invalid message: missing type field");
    expect(() => validateMessage("not an object")).toThrow("Invalid message: missing type field");
  });
});

describe("PeerSession", () => {
  it("send returns false when connection is not open", () => {
    const { conn, session } = createTestSession();
    conn.open = false;
    const result = session.send({ type: "concede" });
    expect(result).toBe(false);
    session.close();
  });

  it("onMessage handler receives parsed messages", () => {
    const { conn, session } = createTestSession();
    const handler = vi.fn();
    session.onMessage(handler);

    const actionMessage = { type: "action" as const, action: { type: "PassPriority" as const } };
    conn.simulateData(actionMessage);

    expect(handler).toHaveBeenCalledTimes(1);
    expect(handler).toHaveBeenCalledWith(actionMessage);
    session.close();
  });

  it("buffers messages when no listeners are attached, then flushes on subscribe", () => {
    const { conn, session } = createTestSession();

    const actionMessage = { type: "action" as const, action: { type: "PassPriority" as const } };
    conn.simulateData(actionMessage);

    const handler = vi.fn();
    session.onMessage(handler);

    expect(handler).toHaveBeenCalledTimes(1);
    expect(handler).toHaveBeenCalledWith(actionMessage);
    session.close();
  });

  it("invokes disconnect handlers immediately if subscribed after disconnect", () => {
    const { destroyPeer, session } = createTestSession();

    session.close("Peer closed");

    const handler = vi.fn();
    session.onDisconnect(handler);

    expect(handler).toHaveBeenCalledTimes(1);
    expect(handler).toHaveBeenCalledWith("Peer closed");
    expect(destroyPeer).toHaveBeenCalledTimes(1);
  });
});
