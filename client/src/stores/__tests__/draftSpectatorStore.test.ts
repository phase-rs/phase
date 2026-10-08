import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useConnectivityStore } from "../connectivityStore";

const mocks = vi.hoisted(() => ({
  detectServerUrl: vi.fn(),
  connectDraftSpectator: vi.fn(),
}));

vi.mock("../../services/serverDetection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/serverDetection")>()),
  detectServerUrl: mocks.detectServerUrl,
}));

vi.mock("../../services/draftSpectatorSession", () => ({
  connectDraftSpectator: mocks.connectDraftSpectator,
}));

import { useDraftSpectatorStore } from "../draftSpectatorStore";

function spectatorSession() {
  const listeners: Array<(event: unknown) => void> = [];
  return {
    close: vi.fn(),
    onEvent: vi.fn((listener: (event: unknown) => void) => {
      listeners.push(listener);
      return () => {
        const index = listeners.indexOf(listener);
        if (index >= 0) listeners.splice(index, 1);
      };
    }),
  };
}

describe("draftSpectatorStore.watchDraft origin routing", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.detectServerUrl.mockResolvedValue("ws://test-server");
    mocks.connectDraftSpectator.mockImplementation(async () => spectatorSession());
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
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

describe("draftSpectatorStore offline gating", () => {
  beforeEach(() => {
    useDraftSpectatorStore.getState().leave();
    vi.clearAllMocks();
    mocks.detectServerUrl.mockResolvedValue("wss://phase.example/ws");
    mocks.connectDraftSpectator.mockResolvedValue(spectatorSession());
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  it.each([
    ["forced offline", { forcedOffline: true, browserOnline: true }],
    ["browser offline", { forcedOffline: false, browserOnline: false }],
  ] as const)("does no cold spectator work while %s", async (_label, connectivity) => {
    useConnectivityStore.setState(connectivity);

    await useDraftSpectatorStore.getState().watchDraft(" abc123 ");

    expect(mocks.detectServerUrl).not.toHaveBeenCalled();
    expect(mocks.connectDraftSpectator).not.toHaveBeenCalled();
    expect(useDraftSpectatorStore.getState()).toMatchObject({
      draftCode: null,
      status: "idle",
      session: null,
    });
  });

  it("preserves an active exact session when an offline replacement is blocked", async () => {
    const active = spectatorSession();
    mocks.connectDraftSpectator.mockResolvedValueOnce(active);
    await useDraftSpectatorStore.getState().watchDraft("abc123");
    useDraftSpectatorStore.setState({ view: {} as never });
    vi.clearAllMocks();
    useConnectivityStore.setState({ forcedOffline: true });

    await useDraftSpectatorStore.getState().watchDraft("fghijk");

    expect(active.close).not.toHaveBeenCalled();
    expect(mocks.detectServerUrl).not.toHaveBeenCalled();
    expect(mocks.connectDraftSpectator).not.toHaveBeenCalled();
    expect(useDraftSpectatorStore.getState()).toMatchObject({
      draftCode: "ABC123",
      view: expect.anything(),
      status: "connecting",
    });
  });

  it("does not connect when held server detection becomes offline", async () => {
    let resolveServer!: (server: string) => void;
    mocks.detectServerUrl.mockImplementationOnce(() => new Promise((resolve) => {
      resolveServer = resolve;
    }));

    const watching = useDraftSpectatorStore.getState().watchDraft("abc123");
    await vi.waitFor(() => expect(mocks.detectServerUrl).toHaveBeenCalledOnce());
    useConnectivityStore.setState({ browserOnline: false });
    resolveServer("wss://phase.example/ws");

    await watching;
    expect(mocks.connectDraftSpectator).not.toHaveBeenCalled();
    expect(useDraftSpectatorStore.getState()).toMatchObject({ draftCode: "ABC123", session: null });
  });

  it("drops a stale online detection before it can connect", async () => {
    let resolveFirst!: (server: string) => void;
    mocks.detectServerUrl
      .mockImplementationOnce(() => new Promise((resolve) => {
        resolveFirst = resolve;
      }))
      .mockResolvedValueOnce("wss://phase.example/ws");

    const first = useDraftSpectatorStore.getState().watchDraft("abc123");
    await vi.waitFor(() => expect(mocks.detectServerUrl).toHaveBeenCalledOnce());
    await useDraftSpectatorStore.getState().watchDraft("fghijk");
    resolveFirst("wss://phase.example/ws");

    await first;
    expect(mocks.connectDraftSpectator).toHaveBeenCalledTimes(1);
    expect(mocks.connectDraftSpectator).toHaveBeenCalledWith("wss://phase.example/ws", "FGHIJK");
    expect(useDraftSpectatorStore.getState().draftCode).toBe("FGHIJK");
  });

  it("publishes a detection failure for the normalized requested code", async () => {
    mocks.detectServerUrl.mockRejectedValueOnce(new Error("Server unavailable"));

    await useDraftSpectatorStore.getState().watchDraft(" abc123 ");

    expect(useDraftSpectatorStore.getState()).toMatchObject({
      draftCode: "ABC123",
      status: "error",
      error: "Server unavailable",
    });
  });
});
