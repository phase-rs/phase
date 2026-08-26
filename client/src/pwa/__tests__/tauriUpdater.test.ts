import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { useMultiplayerDraftStore as MultiplayerDraftStore } from "../../stores/multiplayerDraftStore";

const mocks = vi.hoisted(() => ({
  isDesktopTauri: vi.fn(),
  check: vi.fn(),
  relaunch: vi.fn().mockResolvedValue(undefined),
  markPendingAutoUpdate: vi.fn(),
  claimUpdateStatus: vi.fn(() => true),
  clearUpdateError: vi.fn(),
  pushUpdateDebug: vi.fn(),
  releaseUpdateStatus: vi.fn(),
  setDownloadProgress: vi.fn(),
  setUpdateError: vi.fn(),
  setUpdateStatus: vi.fn(),
}));

vi.mock("../../services/platform", () => ({ isDesktopTauri: mocks.isDesktopTauri }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: mocks.check }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: mocks.relaunch }));
vi.mock("../updateMarker", () => ({ markPendingAutoUpdate: mocks.markPendingAutoUpdate }));
vi.mock("../updateStatus", () => ({
  claimUpdateStatus: mocks.claimUpdateStatus,
  clearUpdateError: mocks.clearUpdateError,
  pushUpdateDebug: mocks.pushUpdateDebug,
  releaseUpdateStatus: mocks.releaseUpdateStatus,
  setDownloadProgress: mocks.setDownloadProgress,
  setUpdateError: mocks.setUpdateError,
  setUpdateStatus: mocks.setUpdateStatus,
}));

describe("registerTauriUpdater", () => {
  const downloadAndInstall = vi.fn().mockResolvedValue(undefined);
  let draftStore: typeof MultiplayerDraftStore;

  beforeEach(async () => {
    vi.resetModules();
    vi.clearAllMocks();
    vi.unstubAllEnvs();
    vi.stubEnv("DEV", false);
    ({ useMultiplayerDraftStore: draftStore } = await import("../../stores/multiplayerDraftStore"));
    downloadAndInstall.mockResolvedValue(undefined);
    mocks.check.mockResolvedValue({
      version: "0.64.0",
      currentVersion: "0.63.0",
      downloadAndInstall,
    });
  });

  afterEach(() => {
    draftStore.setState({ role: null, phase: "idle" });
  });

  it("does not import or invoke the updater on Android/iOS", async () => {
    mocks.isDesktopTauri.mockReturnValue(false);
    const updater = await import("../tauriUpdater");
    updater.registerTauriUpdater();
    expect(updater.checkForTauriUpdate()).toBe(false);
    expect(mocks.check).not.toHaveBeenCalled();
  });

  it("retains desktop updater reachability", async () => {
    mocks.isDesktopTauri.mockReturnValue(true);
    mocks.check.mockResolvedValue(null);
    const updater = await import("../tauriUpdater");
    updater.registerTauriUpdater();
    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledOnce());
  });

  it("does not self-update a dev build", async () => {
    vi.stubEnv("DEV", true);
    mocks.isDesktopTauri.mockReturnValue(true);
    const updater = await import("../tauriUpdater");
    updater.registerTauriUpdater();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(mocks.check).not.toHaveBeenCalled();
    expect(updater.checkForTauriUpdate()).toBe(false);
  });

  it("defers a detected update until the live pod ends and does not start a parallel install", async () => {
    mocks.isDesktopTauri.mockReturnValue(true);
    draftStore.setState({ role: "guest", phase: "pairing" });
    const { checkForTauriUpdate, registerTauriUpdater } = await import("../tauriUpdater");
    registerTauriUpdater();

    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledTimes(1));
    expect(downloadAndInstall).not.toHaveBeenCalled();

    expect(checkForTauriUpdate()).toBe(true);
    expect(checkForTauriUpdate()).toBe(true);
    expect(mocks.check).toHaveBeenCalledTimes(1);

    draftStore.setState({ phase: "complete" });

    await vi.waitFor(() => expect(downloadAndInstall).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(mocks.relaunch).toHaveBeenCalledTimes(1));
  });
});
