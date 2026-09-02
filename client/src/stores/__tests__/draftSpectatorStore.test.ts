import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  connectDraftSpectator: vi.fn(),
  detectServerUrl: vi.fn(async () => "ws://test-server"),
}));

vi.mock("../../services/draftSpectatorSession", () => ({
  connectDraftSpectator: mocks.connectDraftSpectator,
}));

vi.mock("../../services/serverDetection", () => ({
  detectServerUrl: mocks.detectServerUrl,
}));

import { useDraftSpectatorStore } from "../draftSpectatorStore";

function session() {
  return { close: vi.fn(), onEvent: vi.fn(() => () => {}) };
}

describe("draftSpectatorStore.watchDraft", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.detectServerUrl.mockResolvedValue("ws://test-server");
    mocks.connectDraftSpectator.mockImplementation(async () => session());
    useDraftSpectatorStore.getState().leave();
  });

  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it("opens the spectator socket on the route origin", async () => {
    await useDraftSpectatorStore
      .getState()
      .watchDraft("ABC123", "wss://play.example.com/ws");

    expect(mocks.connectDraftSpectator).toHaveBeenCalledWith(
      "wss://play.example.com/ws",
      "ABC123",
    );
    // Reach-guard: the store really got as far as recording the session.
    expect(useDraftSpectatorStore.getState().draftCode).toBe("ABC123");
    expect(useDraftSpectatorStore.getState().status).toBe("connecting");
  });

  it("falls back to the hosting server when the route carried no origin", async () => {
    await useDraftSpectatorStore.getState().watchDraft("ABC123");

    expect(mocks.connectDraftSpectator).toHaveBeenCalledWith(
      "ws://test-server",
      "ABC123",
    );
    expect(mocks.detectServerUrl).toHaveBeenCalledTimes(1);
  });

  it("keeps the VITE_WS_URL override ahead of the route origin", async () => {
    vi.stubEnv("VITE_WS_URL", "ws://forced");

    await useDraftSpectatorStore
      .getState()
      .watchDraft("ABC123", "wss://play.example.com/ws");

    expect(mocks.connectDraftSpectator).toHaveBeenCalledWith("ws://forced", "ABC123");
    expect(mocks.detectServerUrl).not.toHaveBeenCalled();
  });

  it("surfaces an unusable origin through the existing error path", async () => {
    mocks.connectDraftSpectator.mockRejectedValueOnce(
      new Error("Invalid WebSocket URL"),
    );

    await useDraftSpectatorStore.getState().watchDraft("ABC123", "wss:");

    expect(useDraftSpectatorStore.getState().status).toBe("error");
    expect(useDraftSpectatorStore.getState().error).toBe("Invalid WebSocket URL");
  });
});
