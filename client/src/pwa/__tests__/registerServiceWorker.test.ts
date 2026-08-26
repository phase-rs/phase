import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { useMultiplayerDraftStore as MultiplayerDraftStore } from "../../stores/multiplayerDraftStore";

const mocks = vi.hoisted(() => ({
  registerSW: vi.fn(),
  isBundledTauriOrigin: vi.fn(() => false),
  claimServiceWorkerReload: vi.fn(() => true),
  markPendingAutoUpdate: vi.fn(),
  claimUpdateStatus: vi.fn(() => true),
  setUpdateStatus: vi.fn(),
  getUpdateStatus: vi.fn(() => "idle"),
  releaseUpdateStatus: vi.fn(),
  setDownloadProgress: vi.fn(),
  pushUpdateDebug: vi.fn(),
  setUpdateError: vi.fn(),
  clearUpdateError: vi.fn(),
}));

vi.mock("\0virtual:pwa-register-stub", () => ({ registerSW: mocks.registerSW }));
vi.mock("../../services/platform", () => ({ isBundledTauriOrigin: mocks.isBundledTauriOrigin }));
vi.mock("../updateMarker", () => ({
  claimServiceWorkerReload: mocks.claimServiceWorkerReload,
  markPendingAutoUpdate: mocks.markPendingAutoUpdate,
}));
vi.mock("../updateStatus", () => ({
  claimUpdateStatus: mocks.claimUpdateStatus,
  setUpdateStatus: mocks.setUpdateStatus,
  getUpdateStatus: mocks.getUpdateStatus,
  releaseUpdateStatus: mocks.releaseUpdateStatus,
  setDownloadProgress: mocks.setDownloadProgress,
  pushUpdateDebug: mocks.pushUpdateDebug,
  setUpdateError: mocks.setUpdateError,
  clearUpdateError: mocks.clearUpdateError,
}));

type ServiceWorkerOptions = {
  onNeedRefresh(): void;
};

describe("registerServiceWorker draft-pod protection", () => {
  let controllerChange: (() => void) | null;
  let updateSW: ReturnType<typeof vi.fn>;
  let reload: ReturnType<typeof vi.fn>;
  let draftStore: typeof MultiplayerDraftStore;

  beforeEach(async () => {
    vi.resetModules();
    vi.clearAllMocks();
    vi.stubEnv("DEV", false);
    ({ useMultiplayerDraftStore: draftStore } = await import("../../stores/multiplayerDraftStore"));
    controllerChange = null;
    updateSW = vi.fn().mockResolvedValue(undefined);
    mocks.registerSW.mockReturnValue(updateSW);
    Object.defineProperty(navigator, "serviceWorker", {
      configurable: true,
      value: {
        controller: {},
        addEventListener: vi.fn((event: string, listener: () => void) => {
          if (event === "controllerchange") controllerChange = listener;
        }),
      },
    });
    reload = vi.fn();
    Object.defineProperty(window.location, "reload", { configurable: true, value: reload });
    draftStore.setState({ role: "host", phase: "deckbuilding" });
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    draftStore.setState({ role: null, phase: "idle" });
  });

  async function register(): Promise<ServiceWorkerOptions> {
    const { registerServiceWorker } = await import("../registerServiceWorker");
    registerServiceWorker();
    return mocks.registerSW.mock.calls[0][0] as ServiceWorkerOptions;
  }

  it("defers onNeedRefresh through live deckbuilding and applies it once the pod ends", async () => {
    const options = await register();

    options.onNeedRefresh();

    expect(updateSW).not.toHaveBeenCalled();
    draftStore.setState({ phase: "complete" });

    expect(updateSW).toHaveBeenCalledTimes(1);
    expect(updateSW).toHaveBeenCalledWith(true);
  });

  it("defers controllerchange during a live pod and reloads exactly once after release", async () => {
    await register();
    expect(controllerChange).not.toBeNull();

    controllerChange?.();

    expect(reload).not.toHaveBeenCalled();
    draftStore.setState({ phase: "complete" });

    expect(reload).toHaveBeenCalledTimes(1);
    controllerChange?.();
    expect(reload).toHaveBeenCalledTimes(1);
  });
});
