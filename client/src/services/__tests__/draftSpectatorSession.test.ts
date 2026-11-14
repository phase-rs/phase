import { beforeEach, describe, expect, it, vi } from "vitest";

import { connectDraftSpectator } from "../draftSpectatorSession";
import { openPhaseSocket } from "../openPhaseSocket";

vi.mock("../openPhaseSocket", () => ({ openPhaseSocket: vi.fn() }));

const SERVER_URL = "wss://spectate.example/ws";

class MockWebSocket extends EventTarget {
  readyState = 1;
  send = vi.fn();
  close = vi.fn();

  fireClose() {
    this.dispatchEvent(new Event("close"));
  }
}

function mockOpen(ws: MockWebSocket): void {
  vi.mocked(openPhaseSocket).mockResolvedValue({
    ws: ws as unknown as WebSocket,
    serverInfo: {
      version: "0.0.0",
      buildCommit: "test",
      protocolVersion: 1,
      mode: "Full",
    },
    close: () => ws.close(),
  } as unknown as Awaited<ReturnType<typeof openPhaseSocket>>);
}

function framesOfType(ws: MockWebSocket, type: string): { type: string }[] {
  return ws.send.mock.calls
    .map((call) => JSON.parse(call[0] as string) as { type: string })
    .filter((frame) => frame.type === type);
}

beforeEach(() => {
  vi.mocked(openPhaseSocket).mockReset();
});

describe("draft spectator keepalive", () => {
  it("pings a spectator socket that is idle in both directions", async () => {
    vi.useFakeTimers();
    try {
      const ws = new MockWebSocket();
      mockOpen(ws);

      await connectDraftSpectator(SERVER_URL, "ABC123");
      // Reach guard: the session's own subscribe frame is on this mock, so a
      // ping count read below belongs to a socket that really was opened.
      expect(framesOfType(ws, "SpectateDraft")).toHaveLength(1);

      // Nothing is delivered to the socket in between: a draft that is waiting
      // to start or already complete pushes no view after the first.
      await vi.advanceTimersByTimeAsync(11_000);

      expect(framesOfType(ws, "Ping").length).toBeGreaterThanOrEqual(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("stops pinging once the spectator socket closes", async () => {
    vi.useFakeTimers();
    try {
      const ws = new MockWebSocket();
      mockOpen(ws);

      await connectDraftSpectator(SERVER_URL, "ABC123");
      expect(framesOfType(ws, "SpectateDraft")).toHaveLength(1);
      await vi.advanceTimersByTimeAsync(11_000);
      // Reach guard: the interval was running before the close ended it, so a
      // zero below means the stopper fired rather than that it never started.
      expect(framesOfType(ws, "Ping").length).toBeGreaterThanOrEqual(2);

      ws.fireClose();
      ws.send.mockClear();
      await vi.advanceTimersByTimeAsync(11_000);

      expect(framesOfType(ws, "Ping")).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("stops pinging when the session is closed", async () => {
    vi.useFakeTimers();
    try {
      const ws = new MockWebSocket();
      mockOpen(ws);

      const session = await connectDraftSpectator(SERVER_URL, "ABC123");
      expect(framesOfType(ws, "SpectateDraft")).toHaveLength(1);
      await vi.advanceTimersByTimeAsync(11_000);
      // Reach guard: the interval was running before `close()` ended it. The
      // mock raises no close event, so only the explicit stopper can end it.
      expect(framesOfType(ws, "Ping").length).toBeGreaterThanOrEqual(2);

      session.close();
      ws.send.mockClear();
      await vi.advanceTimersByTimeAsync(11_000);

      expect(framesOfType(ws, "Ping")).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });
});
