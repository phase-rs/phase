import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { useMultiplayerDraftStore as MultiplayerDraftStore } from "../../stores/multiplayerDraftStore";

const mocks = vi.hoisted(() => {
  const connectivity = { offline: false, listeners: new Set<(offline: boolean, previous: boolean) => void>() };
  const status = { value: "idle", owner: null as string | null, allowClaim: true };
  return {
    connectivity,
    status,
    isDesktopTauri: vi.fn(),
    check: vi.fn(),
    relaunch: vi.fn().mockResolvedValue(undefined),
    markPendingAutoUpdate: vi.fn(),
    claimUpdateStatus: vi.fn((owner: string) => {
      if (!status.allowClaim) return false;
      if (status.owner !== null && status.owner !== owner) return false;
      status.owner = owner;
      return true;
    }),
    clearUpdateError: vi.fn(),
    getUpdateStatus: vi.fn(() => status.value),
    pushUpdateDebug: vi.fn(),
    releaseUpdateStatus: vi.fn((owner: string) => {
      if (status.owner === owner) status.owner = null;
    }),
    setDownloadProgress: vi.fn(),
    setUpdateError: vi.fn(),
    setUpdateStatus: vi.fn((next: string) => { status.value = next; }),
  };
});

vi.mock("../../services/platform", () => ({ isDesktopTauri: mocks.isDesktopTauri }));
vi.mock("../../stores/connectivityStore", () => ({
  getEffectiveOffline: () => mocks.connectivity.offline,
  subscribeEffectiveOffline: (listener: (offline: boolean, previous: boolean) => void) => {
    mocks.connectivity.listeners.add(listener);
    return () => mocks.connectivity.listeners.delete(listener);
  },
}));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: mocks.check }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: mocks.relaunch }));
vi.mock("../updateMarker", () => ({ markPendingAutoUpdate: mocks.markPendingAutoUpdate }));
vi.mock("../updateStatus", () => ({
  claimUpdateStatus: mocks.claimUpdateStatus,
  clearUpdateError: mocks.clearUpdateError,
  getUpdateStatus: mocks.getUpdateStatus,
  pushUpdateDebug: mocks.pushUpdateDebug,
  releaseUpdateStatus: mocks.releaseUpdateStatus,
  setDownloadProgress: mocks.setDownloadProgress,
  setUpdateError: mocks.setUpdateError,
  setUpdateStatus: mocks.setUpdateStatus,
}));

function transitionOffline(offline: boolean): void {
  const previous = mocks.connectivity.offline;
  if (previous === offline) return;
  mocks.connectivity.offline = offline;
  for (const listener of mocks.connectivity.listeners) listener(offline, previous);
}

describe("registerTauriUpdater connectivity lifecycle", () => {
  const downloadAndInstall = vi.fn().mockResolvedValue(undefined);
  let draftStore: typeof MultiplayerDraftStore;

  beforeEach(async () => {
    vi.resetModules();
    vi.clearAllMocks();
    vi.unstubAllEnvs();
    vi.stubEnv("DEV", false);
    mocks.connectivity.offline = false;
    mocks.status.value = "idle";
    mocks.status.owner = null;
    mocks.connectivity.listeners.clear();
    mocks.isDesktopTauri.mockReturnValue(true);
    mocks.status.allowClaim = true;
    mocks.check.mockResolvedValue(null);
    downloadAndInstall.mockResolvedValue(undefined);
    ({ useMultiplayerDraftStore: draftStore } = await import("../../stores/multiplayerDraftStore"));
  });

  afterEach(async () => {
    const { disposeTauriUpdater } = await import("../tauriUpdater");
    disposeTauriUpdater();
    draftStore.setState({ role: null, phase: "idle" });
    vi.unstubAllEnvs();
  });

  it("does not import or invoke the updater during an offline cold boot", async () => {
    transitionOffline(true);
    const updater = await import("../tauriUpdater");
    updater.registerTauriUpdater();

    expect(mocks.check).not.toHaveBeenCalled();
    expect(updater.checkForTauriUpdate()).toBe(false);
  });

  it("runs one fresh check when connectivity resumes", async () => {
    transitionOffline(true);
    const updater = await import("../tauriUpdater");
    updater.registerTauriUpdater();
    transitionOffline(false);
    transitionOffline(false);

    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledTimes(1));
  });

  it("does not self-update a dev build or non-desktop host", async () => {
    vi.stubEnv("DEV", true);
    const updater = await import("../tauriUpdater");
    updater.registerTauriUpdater();
    expect(mocks.check).not.toHaveBeenCalled();
    expect(updater.checkForTauriUpdate()).toBe(false);
  });

  it("suppresses a stale check result after connectivity pauses", async () => {
    let release!: (value: null) => void;
    mocks.check.mockImplementation(() => new Promise<null>((resolve) => { release = resolve; }));
    const updater = await import("../tauriUpdater");
    updater.registerTauriUpdater();
    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledOnce());

    transitionOffline(true);
    release(null);
    await Promise.resolve();
    expect(mocks.setUpdateStatus).toHaveBeenCalledWith("idle");
    expect(updater.checkForTauriUpdate()).toBe(false);
  });

  it("does not install a non-null result from a pre-pause check after reconnect", async () => {
    let release!: (value: { version: string; currentVersion: string; downloadAndInstall: typeof downloadAndInstall }) => void;
    mocks.check.mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    mocks.check.mockResolvedValueOnce(null);
    const updater = await import("../tauriUpdater");
    updater.registerTauriUpdater();
    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledOnce());

    transitionOffline(true);
    transitionOffline(false);
    release({ version: "0.64.0", currentVersion: "0.63.0", downloadAndInstall });

    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledTimes(2));
    expect(downloadAndInstall).not.toHaveBeenCalled();
  });

  it("coalesces a rapid offline-to-online transition behind the old check", async () => {
    let release!: (value: null) => void;
    mocks.check.mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    mocks.check.mockResolvedValueOnce(null);
    const updater = await import("../tauriUpdater");
    updater.registerTauriUpdater();
    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledOnce());

    transitionOffline(true);
    transitionOffline(false);
    transitionOffline(true);
    transitionOffline(false);
    release(null);

    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledTimes(2));
  });

  it("cancels a multiplayer-deferred install when connectivity goes offline", async () => {
    mocks.check.mockResolvedValue({
      version: "0.64.0",
      currentVersion: "0.63.0",
      downloadAndInstall,
    });
    draftStore.setState({ role: "guest", phase: "pairing" });
    const updater = await import("../tauriUpdater");
    updater.registerTauriUpdater();
    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledOnce());

    transitionOffline(true);
    draftStore.setState({ phase: "complete" });
    await Promise.resolve();

    expect(downloadAndInstall).not.toHaveBeenCalled();
    expect(mocks.setDownloadProgress).toHaveBeenLastCalledWith(0);
    expect(mocks.setUpdateStatus).toHaveBeenLastCalledWith("idle");
    expect(mocks.releaseUpdateStatus).toHaveBeenCalledWith("tauri");
  });

  it("disposes stale work and allows one new lifecycle", async () => {
    const updater = await import("../tauriUpdater");
    updater.registerTauriUpdater();
    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledOnce());
    updater.disposeTauriUpdater();
    updater.registerTauriUpdater();

    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledTimes(2));
  });

  it("does not let an old unload callback dispose a replacement lifecycle", async () => {
    const addEventListener = vi.spyOn(window, "addEventListener");
    const updater = await import("../tauriUpdater");
    updater.registerTauriUpdater();
    const oldUnload = addEventListener.mock.calls.find(([event]) => event === "beforeunload")?.[1] as EventListener;
    updater.disposeTauriUpdater();
    updater.registerTauriUpdater();
    oldUnload(new Event("beforeunload"));

    expect(updater.checkForTauriUpdate()).toBe(true);
  });

  it("preserves another updater's active download status while pausing", async () => {
    mocks.status.value = "downloading";
    mocks.status.owner = "serviceWorker";
    mocks.status.allowClaim = false;
    const updater = await import("../tauriUpdater");
    updater.registerTauriUpdater();
    vi.clearAllMocks();

    transitionOffline(true);
    expect(mocks.releaseUpdateStatus).not.toHaveBeenCalled();
    expect(mocks.setUpdateStatus).not.toHaveBeenCalled();
    expect(mocks.status.owner).toBe("serviceWorker");
  });

  it("does not check or install through a replacement module while an old install is running", async () => {
    let finishInstall!: () => void;
    const runningInstall = vi.fn(() => new Promise<void>((resolve) => { finishInstall = resolve; }));
    mocks.check.mockResolvedValueOnce({
      version: "0.64.0",
      currentVersion: "0.63.0",
      downloadAndInstall: runningInstall,
    });
    const first = await import("../tauriUpdater");
    first.registerTauriUpdater();
    await vi.waitFor(() => expect(runningInstall).toHaveBeenCalledOnce());

    vi.clearAllMocks();
    first.disposeTauriUpdater();
    expect(mocks.releaseUpdateStatus).not.toHaveBeenCalled();
    vi.resetModules();
    const replacement = await import("../tauriUpdater");
    replacement.registerTauriUpdater();
    await Promise.resolve();

    expect(mocks.check).not.toHaveBeenCalled();
    finishInstall();
    await vi.waitFor(() => expect(mocks.markPendingAutoUpdate).toHaveBeenCalledOnce());
    await vi.waitFor(() => expect(mocks.relaunch).toHaveBeenCalledOnce());
    expect(mocks.releaseUpdateStatus).toHaveBeenCalledTimes(1);
    expect(mocks.status.owner).toBeNull();
    expect(replacement.checkForTauriUpdate()).toBe(true);
    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledTimes(1));
  });

  it("does not start an unowned install after a module replacement", async () => {
    mocks.status.allowClaim = false;
    mocks.check.mockResolvedValue({
      version: "0.64.0",
      currentVersion: "0.63.0",
      downloadAndInstall,
    });
    const first = await import("../tauriUpdater");
    first.registerTauriUpdater();
    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledOnce());
    first.disposeTauriUpdater();
    vi.resetModules();
    const replacement = await import("../tauriUpdater");
    replacement.registerTauriUpdater();
    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledTimes(2));

    expect(downloadAndInstall).not.toHaveBeenCalled();
  });

  it("drops a non-null check result that resolves after disposal", async () => {
    let release!: (value: { version: string; currentVersion: string; downloadAndInstall: typeof downloadAndInstall }) => void;
    mocks.check.mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    mocks.check.mockResolvedValueOnce(null);
    const first = await import("../tauriUpdater");
    first.registerTauriUpdater();
    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledOnce());
    first.disposeTauriUpdater();
    vi.resetModules();
    const replacement = await import("../tauriUpdater");
    replacement.registerTauriUpdater();
    release({ version: "0.64.0", currentVersion: "0.63.0", downloadAndInstall });

    await vi.waitFor(() => expect(mocks.check).toHaveBeenCalledTimes(2));
    expect(downloadAndInstall).not.toHaveBeenCalled();
  });

  it("keeps exactly one scheduler per online epoch and suppresses offline intervals", async () => {
    vi.useFakeTimers();
    const setInterval = vi.spyOn(window, "setInterval");
    try {
      const updater = await import("../tauriUpdater");
      updater.registerTauriUpdater();
      expect(setInterval).toHaveBeenCalledTimes(1);

      transitionOffline(true);
      vi.clearAllMocks();
      vi.advanceTimersByTime(60 * 60 * 1000);
      expect(mocks.check).not.toHaveBeenCalled();

      transitionOffline(false);
      expect(setInterval).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });
});
