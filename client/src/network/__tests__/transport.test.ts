import { beforeEach, describe, expect, it, vi } from "vitest";

const peerState = vi.hoisted(() => ({ constructed: [] as unknown[][] }));

vi.mock("peerjs", () => ({
  default: class FakePeer {
    id = "peerjs-default";
    destroyed = false;
    disconnected = false;
    constructor(...args: unknown[]) { peerState.constructed.push(args); }
    connect() { throw new Error("not used by this test"); }
    destroy() {}
    reconnect() {}
    on() { return this; }
    once() { return this; }
    off() { return this; }
  },
}));

beforeEach(() => vi.resetModules());

async function freshTransport() {
  peerState.constructed.length = 0;
  return import("../transport");
}

const context = { role: "guest", hostPeerId: "phase2-ABCDE" } as const;

describe("peer transport selection", () => {
  it("keeps PeerJS as the default and locks selection before a late install", async () => {
    const transport = await freshTransport();

    const selected = transport.selectPeerTransportFactory(context);

    expect(selected).toBe(transport.peerTransportFactory);
    const peer = transport.createPeer(undefined, { config: { iceServers: [] } }, selected);
    expect(peer).toMatchObject({ id: "peerjs-default" });
    expect(peerState.constructed).toEqual([[{ config: { iceServers: [] } }]]);
    expect(() => transport.installPeerTransportSelector(() => selected)).toThrow("selection is already locked");
  });

  it("rejects duplicate installation and locks before invoking a reentrant selector", async () => {
    const transport = await freshTransport();
    const factory = { create: vi.fn() } as unknown as typeof transport.peerTransportFactory;
    const seen: unknown[] = [];
    transport.installPeerTransportSelector((received) => {
      seen.push(received);
      expect(() => transport.installPeerTransportSelector(() => factory)).toThrow("selection is already locked");
      return factory;
    });
    expect(() => transport.installPeerTransportSelector(() => factory)).toThrow("selector is already installed");

    expect(transport.selectPeerTransportFactory(context)).toBe(factory);
    expect(seen).toHaveLength(1);
    expect(seen[0]).toEqual(context);
    expect(Object.isFrozen(seen[0])).toBe(true);
  });

  it("keeps the one-time lock after a selector throws", async () => {
    const transport = await freshTransport();
    const failure = new Error("selector failed");
    transport.installPeerTransportSelector(() => { throw failure; });

    expect(() => transport.selectPeerTransportFactory(context)).toThrow(failure);
    expect(() => transport.installPeerTransportSelector(() => transport.peerTransportFactory)).toThrow("selection is already locked");
  });

  it("lets an explicit factory bypass the selector and shared lock", async () => {
    const transport = await freshTransport();
    const override = { create: vi.fn() } as unknown as typeof transport.peerTransportFactory;
    const selected = { create: vi.fn() } as unknown as typeof transport.peerTransportFactory;
    const selector = vi.fn(() => selected);

    expect(transport.selectPeerTransportFactory(context, override)).toBe(override);
    transport.installPeerTransportSelector(selector);
    expect(transport.selectPeerTransportFactory(context)).toBe(selected);
    expect(selector).toHaveBeenCalledTimes(1);
    expect(selector).toHaveBeenCalledWith(context);
  });
});
