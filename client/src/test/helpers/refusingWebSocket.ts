import { vi } from "vitest";

/**
 * Stubs the global `WebSocket` with a constructor that records the URL it
 * was given and throws instead of opening a connection. Every suite that
 * drives a join/host path through a mocked store must install this so a
 * lookup or host attempt that reaches past its mocks fails loudly instead of
 * opening a socket to the production lobby.
 *
 * Returns the array the stub pushes each attempted URL into; callers assert
 * it is empty in their `afterEach`, captured before `vi.unstubAllGlobals()`
 * removes the stub.
 */
export function refuseRealWebSockets(): string[] {
  const opened: string[] = [];
  class RefusingWebSocket {
    static readonly CONNECTING = 0;
    static readonly OPEN = 1;
    static readonly CLOSING = 2;
    static readonly CLOSED = 3;
    constructor(url: string | URL) {
      opened.push(String(url));
      throw new Error("a real WebSocket must never open in this suite");
    }
  }
  vi.stubGlobal("WebSocket", RefusingWebSocket);
  return opened;
}
