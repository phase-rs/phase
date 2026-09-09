import { describe, expect, it, vi } from "vitest";

import { KEEPALIVE_INTERVAL_MS, startSocketKeepalive } from "../socketKeepalive";

describe("socket keepalive", () => {
  function fakeSocket(readyState: number) {
    return { readyState, send: vi.fn() };
  }

  it("clears its own interval when the socket dies without a close event", async () => {
    // Skipping the send and clearing the interval both produce zero pings, so
    // the pending-timer count is what separates them.
    vi.useFakeTimers();
    try {
      const ws = fakeSocket(WebSocket.OPEN);
      startSocketKeepalive(ws);
      await vi.advanceTimersByTimeAsync(KEEPALIVE_INTERVAL_MS * 2);
      expect(ws.send).toHaveBeenCalled();
      expect(vi.getTimerCount()).toBe(1);

      ws.readyState = WebSocket.CLOSED;
      await vi.advanceTimersByTimeAsync(KEEPALIVE_INTERVAL_MS);
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps pinging a socket that stays open", async () => {
    // Control for the row above: a keepalive that cleared its interval on any
    // tick would satisfy that one on its own.
    vi.useFakeTimers();
    try {
      const ws = fakeSocket(WebSocket.OPEN);
      startSocketKeepalive(ws);
      await vi.advanceTimersByTimeAsync(KEEPALIVE_INTERVAL_MS * 3);

      expect(ws.send.mock.calls.length).toBeGreaterThanOrEqual(3);
      expect(vi.getTimerCount()).toBe(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("holds its interval and sends nothing while the socket is still connecting", async () => {
    // Without this row the `!== OPEN` return reads as dead code once the
    // self-clear is in place, and deleting it would send on a CONNECTING
    // socket — an InvalidStateError thrown inside the interval every tick.
    vi.useFakeTimers();
    try {
      const ws = fakeSocket(WebSocket.CONNECTING);
      startSocketKeepalive(ws);
      await vi.advanceTimersByTimeAsync(KEEPALIVE_INTERVAL_MS);

      expect(ws.send).not.toHaveBeenCalled();
      expect(vi.getTimerCount()).toBe(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("stops when its owner calls the stopper", async () => {
    vi.useFakeTimers();
    try {
      const ws = fakeSocket(WebSocket.OPEN);
      const stop = startSocketKeepalive(ws);
      await vi.advanceTimersByTimeAsync(KEEPALIVE_INTERVAL_MS);
      expect(vi.getTimerCount()).toBe(1);

      stop();
      stop();
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });
});
