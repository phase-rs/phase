import { afterEach, describe, expect, it, vi } from "vitest";

import { refuseRealWebSockets } from "../refusingWebSocket";

describe("refuseRealWebSockets", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("throws instead of opening a socket, and records the attempted URL", () => {
    const opened = refuseRealWebSockets();

    expect(() => new WebSocket("wss://probe.invalid/ws")).toThrow(
      "a real WebSocket must never open in this suite",
    );
    expect(opened).toEqual(["wss://probe.invalid/ws"]);
  });
});
