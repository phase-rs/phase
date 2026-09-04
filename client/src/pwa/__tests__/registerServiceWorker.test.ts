import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { useMultiplayerDraftStore as MultiplayerDraftStore } from "../../stores/multiplayerDraftStore";

const mocks = vi.hoisted(() => {
  const connectivity = { offline: false, listeners: new Set<(offline: boolean, previous: boolean) => void>() };
  const status = { value: "idle", owner: null as string | null, allowClaim: true };
  return {
    connectivity,
    status,
    registerSW: vi.fn(),
    isBundledTauriOrigin: vi.fn(() => false),
    claimServiceWorkerReload: vi.fn(() => true),
    markPendingAutoUpdate: vi.fn(),
    claimUpdateStatus: vi.fn((owner: string) => {
      if (!status.allowClaim) return false;
      if (status.owner !== null && status.owner !== owner) return false;
      status.owner = owner;
      return true;
    }),
    setUpdateStatus: vi.fn((next: string) => { status.value = next; }),
    getUpdateStatus: vi.fn(() => status.value),
    releaseUpdateStatus: vi.fn((owner: string) => {
      if (status.owner === owner) status.owner = null;
    }),
    setDownloadProgress: vi.fn(),
    pushUpdateDebug: vi.fn(),
    setUpdateError: vi.fn(),
    clearUpdateError: vi.fn(),
    markRemoteLoadOk: vi.fn(() => Promise.resolve(true)),
  };
});

vi.mock("\0virtual:pwa-register-stub", () => ({ registerSW: mocks.registerSW }));
vi.mock("../../services/platform", () => ({ isBundledTauriOrigin: mocks.isBundledTauriOrigin }));
vi.mock("../../services/legacyMigration", () => ({ markRemoteLoadOk: mocks.markRemoteLoadOk }));
vi.mock("../../stores/connectivityStore", () => ({
  getEffectiveOffline: () => mocks.connectivity.offline,
  subscribeEffectiveOffline: (listener: (offline: boolean, previous: boolean) => void) => {
    mocks.connectivity.listeners.add(listener);
    return () => mocks.connectivity.listeners.delete(listener);
  },
}));
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
  onNeedReload(): void;
  onRegisteredSW(url: string, registration: ServiceWorkerRegistration | undefined): void;
  onRegisterError(error: unknown): void;
};

function transitionOffline(offline: boolean): void {
  const previous = mocks.connectivity.offline;
  if (previous === offline) return;
  mocks.connectivity.offline = offline;
  for (const listener of mocks.connectivity.listeners) listener(offline, previous);
}

describe("registerServiceWorker connectivity lifecycle", () => {
  let draftStore: typeof MultiplayerDraftStore;
  let reload: ReturnType<typeof vi.fn>;

  beforeEach(async () => {
    vi.resetModules();
    vi.clearAllMocks();
    vi.stubEnv("DEV", false);
    mocks.connectivity.offline = false;
    mocks.status.value = "idle";
    mocks.status.owner = null;
    mocks.connectivity.listeners.clear();
    mocks.status.allowClaim = true;
    mocks.claimServiceWorkerReload.mockReturnValue(true);
    mocks.markRemoteLoadOk.mockResolvedValue(true);
    ({ useMultiplayerDraftStore: draftStore } = await import("../../stores/multiplayerDraftStore"));
    Object.defineProperty(navigator, "serviceWorker", {
      configurable: true,
      value: { controller: { scriptURL: "/sw.js" } },
    });
    reload = vi.fn();
    Object.defineProperty(window.location, "reload", { configurable: true, value: reload });
    Object.defineProperty(window, "isSecureContext", { configurable: true, value: true });
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ status: 200 }));
  });

  afterEach(async () => {
    const { disposeServiceWorkerUpdater } = await import("../registerServiceWorker");
    disposeServiceWorkerUpdater();
    draftStore.setState({ role: null, phase: "idle" });
    vi.unstubAllEnvs();
    vi.unstubAllGlobals();
  });

  async function register(): Promise<ServiceWorkerOptions> {
    const { registerServiceWorker } = await import("../registerServiceWorker");
    registerServiceWorker();
    return mocks.registerSW.mock.calls[0]?.[0] as ServiceWorkerOptions;
  }

  function registration(): ServiceWorkerRegistration {
    return {
      scope: "/",
      installing: null,
      update: vi.fn().mockResolvedValue(undefined),
      addEventListener: vi.fn(),
    } as unknown as ServiceWorkerRegistration;
  }

  function activeWorker(state: ServiceWorkerState = "activated"): ServiceWorker {
    return { state } as ServiceWorker;
  }

  function jsonResponse(payload: unknown, status = 200): Response {
    return new Response(JSON.stringify(payload), {
      status,
      headers: { "content-type": "application/json" },
    });
  }

  function malformedJsonResponse(): Response {
    return new Response("{", { headers: { "content-type": "application/json" } });
  }

  async function readyRegistration(): Promise<ServiceWorkerRegistration> {
    const options = await register();
    const active = activeWorker();
    Object.defineProperty(navigator, "serviceWorker", {
      configurable: true,
      value: { controller: active },
    });
    const ready = { ...registration(), active, waiting: null } as ServiceWorkerRegistration;
    options.onRegisteredSW("/sw.js", ready);
    await vi.waitFor(() => expect(ready.update).toHaveBeenCalled());
    vi.mocked(fetch).mockClear();
    return ready;
  }

  it("keeps an existing controller and makes no registration attempt during offline cold boot", async () => {
    const controller = navigator.serviceWorker.controller;
    transitionOffline(true);
    const { registerServiceWorker, checkForServiceWorkerUpdate } = await import("../registerServiceWorker");
    registerServiceWorker();

    expect(mocks.registerSW).not.toHaveBeenCalled();
    expect(navigator.serviceWorker.controller).toBe(controller);
    expect(checkForServiceWorkerUpdate()).toBe(false);
  });

  it("registers once on the first online transition", async () => {
    transitionOffline(true);
    await register();
    transitionOffline(false);
    transitionOffline(false);

    expect(mocks.registerSW).toHaveBeenCalledTimes(1);
  });

  it("suppresses manual and automatic checks while offline", async () => {
    const options = await register();
    const swRegistration = registration();
    options.onRegisteredSW("/sw.js", swRegistration);
    await vi.waitFor(() => expect(swRegistration.update).toHaveBeenCalledTimes(1));

    transitionOffline(true);
    const { checkForServiceWorkerUpdate } = await import("../registerServiceWorker");
    expect(checkForServiceWorkerUpdate()).toBe(false);
    document.dispatchEvent(new Event("visibilitychange"));
    await Promise.resolve();

    expect(swRegistration.update).toHaveBeenCalledTimes(1);
  });

  it("retries one failed registration after reconnect while the first attempt was unresolved", async () => {
    const first = await register();
    transitionOffline(true);
    transitionOffline(false);
    first.onRegisterError(new Error("temporary"));

    expect(mocks.registerSW).toHaveBeenCalledTimes(2);
    const second = mocks.registerSW.mock.calls[1][0] as ServiceWorkerOptions;
    second.onRegisterError(new Error("still failing"));
    expect(mocks.registerSW).toHaveBeenCalledTimes(2);
  });

  it("waits for reconnect when an unresolved registration fails while offline", async () => {
    const first = await register();
    transitionOffline(true);
    first.onRegisterError(new Error("offline failure"));
    expect(mocks.registerSW).toHaveBeenCalledTimes(1);

    transitionOffline(false);
    expect(mocks.registerSW).toHaveBeenCalledTimes(2);
  });

  it("does not let a stale failed attempt reload after its successor registered", async () => {
    const first = await register();
    transitionOffline(true);
    transitionOffline(false);
    first.onRegisterError(new Error("temporary"));
    const second = mocks.registerSW.mock.calls[1][0] as ServiceWorkerOptions;
    second.onRegisteredSW("/sw.js", registration());

    first.onNeedReload();
    expect(reload).not.toHaveBeenCalled();
    expect(mocks.markPendingAutoUpdate).not.toHaveBeenCalled();
  });

  it("invalidates a pre-pause probe even when it resolves after reconnect", async () => {
    let resolveFetch!: (value: { status: number }) => void;
    let signal: AbortSignal | undefined;
    vi.stubGlobal("fetch", vi.fn((_url, init: RequestInit) => {
      signal = init.signal as AbortSignal;
      return new Promise((resolve) => { resolveFetch = resolve; });
    }));
    const options = await register();
    const swRegistration = registration();
    options.onRegisteredSW("/sw.js", swRegistration);
    await vi.waitFor(() => expect(fetch).toHaveBeenCalledOnce());

    transitionOffline(true);
    expect(signal?.aborted).toBe(true);
    transitionOffline(false);
    resolveFetch({ status: 200 });
    await vi.waitFor(() => expect(fetch).toHaveBeenCalledTimes(2));

    expect(swRegistration.update).not.toHaveBeenCalled();
  });

  it("coalesces overlapping manual checks behind one active probe", async () => {
    const resolves: Array<(value: { status: number }) => void> = [];
    vi.stubGlobal("fetch", vi.fn(() => new Promise<{ status: number }>((resolve) => { resolves.push(resolve); })));
    const options = await register();
    const swRegistration = registration();
    options.onRegisteredSW("/sw.js", swRegistration);
    await vi.waitFor(() => expect(fetch).toHaveBeenCalledOnce());
    const { checkForServiceWorkerUpdate } = await import("../registerServiceWorker");
    expect(checkForServiceWorkerUpdate()).toBe(true);
    expect(checkForServiceWorkerUpdate()).toBe(true);

    resolves.shift()?.({ status: 200 });
    await vi.waitFor(() => expect(swRegistration.update).toHaveBeenCalledTimes(2));
    expect(fetch).toHaveBeenCalledOnce();
  });

  it("does not run an active or queued check after a reload becomes multiplayer-deferred", async () => {
    let resolveFetch!: (value: { status: number }) => void;
    vi.stubGlobal("fetch", vi.fn(() => new Promise((resolve) => { resolveFetch = resolve; })));
    const options = await register();
    const swRegistration = registration();
    options.onRegisteredSW("/sw.js", swRegistration);
    await vi.waitFor(() => expect(fetch).toHaveBeenCalledOnce());
    const { checkForServiceWorkerUpdate } = await import("../registerServiceWorker");
    checkForServiceWorkerUpdate();
    draftStore.setState({ role: "host", phase: "deckbuilding" });
    options.onNeedReload();
    resolveFetch({ status: 200 });
    await Promise.resolve();

    expect(swRegistration.update).not.toHaveBeenCalled();
    expect(fetch).toHaveBeenCalledOnce();
  });

  it("suppresses a deferred reload race even when the badge claim is rejected", async () => {
    mocks.status.allowClaim = false;
    let resolveFetch!: (value: { status: number }) => void;
    vi.stubGlobal("fetch", vi.fn(() => new Promise((resolve) => { resolveFetch = resolve; })));
    const options = await register();
    const swRegistration = registration();
    options.onRegisteredSW("/sw.js", swRegistration);
    await vi.waitFor(() => expect(fetch).toHaveBeenCalledOnce());
    const { checkForServiceWorkerUpdate } = await import("../registerServiceWorker");
    checkForServiceWorkerUpdate();
    draftStore.setState({ role: "host", phase: "deckbuilding" });
    options.onNeedReload();
    resolveFetch({ status: 200 });
    await Promise.resolve();

    expect(swRegistration.update).not.toHaveBeenCalled();
    expect(fetch).toHaveBeenCalledOnce();
  });

  it("upgrades a queued automatic request to a script probe behind an active manual check", async () => {
    const updates: Array<() => void> = [];
    const swRegistration = registration();
    swRegistration.update = vi.fn(() => new Promise<void>((resolve) => { updates.push(resolve); }));
    const options = await register();
    options.onRegisteredSW("/sw.js", swRegistration);
    await vi.waitFor(() => expect(fetch).toHaveBeenCalledOnce());
    updates.shift()?.();
    await vi.waitFor(() => expect(swRegistration.update).toHaveBeenCalledTimes(1));

    const { checkForServiceWorkerUpdate } = await import("../registerServiceWorker");
    checkForServiceWorkerUpdate();
    await vi.waitFor(() => expect(swRegistration.update).toHaveBeenCalledTimes(2));
    document.dispatchEvent(new Event("visibilitychange"));
    updates.shift()?.();

    await vi.waitFor(() => expect(fetch).toHaveBeenCalledTimes(2));
  });

  it("holds a multiplayer-deferred reload while offline and resubmits it once online", async () => {
    const options = await register();
    options.onRegisteredSW("/sw.js", registration());
    draftStore.setState({ role: "host", phase: "deckbuilding" });
    options.onNeedReload();

    expect(mocks.status.value).toBe("deferred");
    expect(mocks.status.owner).toBe("serviceWorker");
    expect(mocks.releaseUpdateStatus).not.toHaveBeenCalled();

    transitionOffline(true);
    draftStore.setState({ phase: "complete" });
    expect(reload).not.toHaveBeenCalled();
    expect(mocks.markPendingAutoUpdate).not.toHaveBeenCalled();

    transitionOffline(false);
    expect(reload).toHaveBeenCalledTimes(1);
    expect(mocks.markPendingAutoUpdate).toHaveBeenCalledTimes(1);
  });

  it("does not mark or reload when the reload claim is rejected", async () => {
    mocks.claimServiceWorkerReload.mockReturnValue(false);
    const options = await register();
    options.onRegisteredSW("/sw.js", registration());
    options.onNeedReload();

    expect(reload).not.toHaveBeenCalled();
    expect(mocks.markPendingAutoUpdate).not.toHaveBeenCalled();
    expect(mocks.releaseUpdateStatus).toHaveBeenCalledWith("serviceWorker");
  });

  it("releases an unclaimed installation latch at its terminal state", async () => {
    mocks.status.allowClaim = false;
    let updateFound: (() => void) | undefined;
    let stateChange: (() => void) | undefined;
    const worker = {
      state: "installing",
      addEventListener: vi.fn((event: string, listener: () => void) => {
        if (event === "statechange") stateChange = listener;
      }),
    };
    const swRegistration = {
      scope: "/",
      installing: worker as unknown as ServiceWorker,
      update: vi.fn().mockResolvedValue(undefined),
      addEventListener: vi.fn((event: string, listener: () => void) => {
        if (event === "updatefound") updateFound = listener;
      }),
    } as unknown as ServiceWorkerRegistration;
    const options = await register();
    options.onRegisteredSW("/sw.js", swRegistration);
    updateFound?.();
    worker.state = "activated";
    stateChange?.();
    updateFound?.();

    expect(worker.addEventListener).toHaveBeenCalledTimes(2);
  });

  it("does not let an old unload callback dispose its replacement lifecycle", async () => {
    const addEventListener = vi.spyOn(window, "addEventListener");
    await register();
    const oldUnload = addEventListener.mock.calls.find(([event]) => event === "beforeunload")?.[1] as EventListener;
    const { disposeServiceWorkerUpdater, registerServiceWorker, checkForServiceWorkerUpdate } = await import("../registerServiceWorker");
    disposeServiceWorkerUpdater();
    registerServiceWorker();
    const replacement = mocks.registerSW.mock.calls[1][0] as ServiceWorkerOptions;
    replacement.onRegisteredSW("/sw.js", registration());

    oldUnload(new Event("beforeunload"));
    expect(checkForServiceWorkerUpdate()).toBe(true);
  });

  it("keeps an installation started before disposal responsible for its own terminal settlement", async () => {
    let updateFound: (() => void) | undefined;
    let stateChange: (() => void) | undefined;
    const worker = {
      state: "installing",
      addEventListener: vi.fn((event: string, listener: () => void) => {
        if (event === "statechange") stateChange = listener;
      }),
    };
    const swRegistration = {
      scope: "/",
      installing: worker as unknown as ServiceWorker,
      update: vi.fn().mockResolvedValue(undefined),
      addEventListener: vi.fn((event: string, listener: () => void) => {
        if (event === "updatefound") updateFound = listener;
      }),
    } as unknown as ServiceWorkerRegistration;
    const options = await register();
    options.onRegisteredSW("/sw.js", swRegistration);
    updateFound?.();
    vi.clearAllMocks();
    const { disposeServiceWorkerUpdater } = await import("../registerServiceWorker");
    disposeServiceWorkerUpdater();
    expect(mocks.releaseUpdateStatus).not.toHaveBeenCalled();
    worker.state = "activated";
    stateChange?.();

    expect(mocks.releaseUpdateStatus).toHaveBeenCalledTimes(1);
    expect(mocks.releaseUpdateStatus).toHaveBeenCalledWith("serviceWorker");
    expect(mocks.status.owner).toBeNull();
  });

  it("ignores updatefound from a disposed registration", async () => {
    let updateFound: (() => void) | undefined;
    const swRegistration = {
      scope: "/",
      installing: { state: "installing", addEventListener: vi.fn() } as unknown as ServiceWorker,
      update: vi.fn().mockResolvedValue(undefined),
      addEventListener: vi.fn((event: string, listener: () => void) => {
        if (event === "updatefound") updateFound = listener;
      }),
    } as unknown as ServiceWorkerRegistration;
    const options = await register();
    options.onRegisteredSW("/sw.js", swRegistration);
    const { disposeServiceWorkerUpdater } = await import("../registerServiceWorker");
    disposeServiceWorkerUpdater();
    vi.clearAllMocks();
    updateFound?.();

    expect(mocks.claimUpdateStatus).not.toHaveBeenCalled();
  });

  it("does not release another updater's active status while pausing", async () => {
    mocks.status.value = "downloading";
    mocks.status.owner = "tauri";
    mocks.status.allowClaim = false;
    const options = await register();
    options.onRegisteredSW("/sw.js", registration());
    vi.clearAllMocks();

    transitionOffline(true);
    expect(mocks.releaseUpdateStatus).not.toHaveBeenCalled();
    expect(mocks.setUpdateStatus).not.toHaveBeenCalled();
    expect(mocks.status.owner).toBe("tauri");
  });

  it("does not publish a probe result after disposal and replacement", async () => {
    let resolveFetch!: (value: { status: number }) => void;
    vi.stubGlobal("fetch", vi.fn()
      .mockImplementationOnce(() => new Promise((resolve) => { resolveFetch = resolve; }))
      .mockResolvedValue({ status: 200 }));
    const first = await register();
    const firstRegistration = registration();
    first.onRegisteredSW("/sw.js", firstRegistration);
    await vi.waitFor(() => expect(fetch).toHaveBeenCalledOnce());
    const { disposeServiceWorkerUpdater, registerServiceWorker } = await import("../registerServiceWorker");
    disposeServiceWorkerUpdater();
    registerServiceWorker();
    const replacement = mocks.registerSW.mock.calls[1][0] as ServiceWorkerOptions;
    replacement.onRegisteredSW("/sw.js", registration());
    resolveFetch({ status: 200 });
    await Promise.resolve();

    expect(firstRegistration.update).not.toHaveBeenCalled();
  });

  it("keeps exactly one scheduler per online epoch and suppresses offline intervals", async () => {
    vi.useFakeTimers();
    const setInterval = vi.spyOn(window, "setInterval");
    try {
      const options = await register();
      options.onRegisteredSW("/sw.js", registration());
      expect(setInterval).toHaveBeenCalledTimes(1);

      transitionOffline(true);
      vi.clearAllMocks();
      vi.advanceTimersByTime(60 * 60 * 1000);
      expect(fetch).not.toHaveBeenCalled();

      transitionOffline(false);
      expect(setInterval).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("disposal makes stale callbacks inert and permits exactly one replacement lifecycle", async () => {
    const options = await register();
    const { disposeServiceWorkerUpdater, registerServiceWorker } = await import("../registerServiceWorker");
    disposeServiceWorkerUpdater();
    options.onRegisteredSW("/sw.js", registration());
    options.onNeedReload();
    options.onRegisterError(new Error("stale"));
    registerServiceWorker();

    expect(mocks.registerSW).toHaveBeenCalledTimes(2);
    expect(reload).not.toHaveBeenCalled();
  });

  describe("app-shell readiness", () => {
    it("reports missing browser and registration prerequisites with stable reasons", async () => {
      const { checkAppShellReadiness } = await import("../registerServiceWorker");
      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "not-ready", reason: "lifecycle-unavailable" });

      Object.defineProperty(window, "isSecureContext", { configurable: true, value: false });
      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "not-ready", reason: "insecure-context" });

      Object.defineProperty(window, "isSecureContext", { configurable: true, value: true });
      Reflect.deleteProperty(navigator, "serviceWorker");
      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "not-ready", reason: "service-worker-unsupported" });
    });

    it("reports missing active workers and controllers before cache work", async () => {
      const entry = await readyRegistration();
      const { checkAppShellReadiness } = await import("../registerServiceWorker");
      Object.assign(entry, { active: null });
      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "not-ready", reason: "active-worker-unavailable" });

      const active = activeWorker();
      Object.assign(entry, { active });
      Object.defineProperty(navigator, "serviceWorker", { configurable: true, value: { controller: null } });
      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "not-ready", reason: "controller-unavailable" });
      expect(fetch).not.toHaveBeenCalled();
    });

    it("requires the active current controller, cache-only marker, and awaited remote marker", async () => {
      const entry = await readyRegistration();
      const { checkAppShellReadiness } = await import("../registerServiceWorker");
      let resolveMarker!: (value: boolean) => void;
      mocks.markRemoteLoadOk.mockImplementationOnce(() => new Promise<boolean>((resolve) => { resolveMarker = resolve; }));
      vi.mocked(fetch).mockResolvedValueOnce(jsonResponse({ build: __BUILD_HASH__ }));

      const readiness = checkAppShellReadiness();
      await vi.waitFor(() => expect(mocks.markRemoteLoadOk).toHaveBeenCalledOnce());
      await expect(Promise.race([readiness, Promise.resolve("pending")])).resolves.toBe("pending");
      resolveMarker(true);

      await expect(readiness).resolves.toEqual({ status: "ready" });
      expect(fetch).toHaveBeenCalledWith(
        expect.stringMatching(new RegExp(`^/offline-shell-${__BUILD_HASH__}\\.json\\?phase-precache-probe=`)),
        { mode: "same-origin", cache: "only-if-cached" },
      );
      expect(mocks.registerSW).toHaveBeenCalledTimes(1);
      expect(entry.update).toHaveBeenCalledTimes(1);
    });

    it("uses a distinct cache-only marker URL for sequential readiness checks", async () => {
      await readyRegistration();
      const { checkAppShellReadiness } = await import("../registerServiceWorker");
      vi.mocked(fetch).mockResolvedValue(jsonResponse({ build: __BUILD_HASH__ }));

      await checkAppShellReadiness();
      await checkAppShellReadiness();

      const [firstUrl] = vi.mocked(fetch).mock.calls[0];
      const [secondUrl] = vi.mocked(fetch).mock.calls[1];
      expect(firstUrl).not.toBe(secondUrl);
    });

    it.each([
      ["installing", (entry: ServiceWorkerRegistration) => { Object.assign(entry, { installing: activeWorker("installing") }); }],
      ["waiting", (entry: ServiceWorkerRegistration) => { Object.assign(entry, { waiting: activeWorker("installed") }); }],
      ["activating", (entry: ServiceWorkerRegistration) => { Object.assign(entry.active!, { state: "activating" }); }],
    ])("returns update-in-progress when a worker is %s", async (_state, mutate) => {
      const entry = await readyRegistration();
      mutate(entry);
      const { checkAppShellReadiness } = await import("../registerServiceWorker");

      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "not-ready", reason: "update-in-progress" });
      expect(fetch).not.toHaveBeenCalled();
      expect(mocks.markRemoteLoadOk).not.toHaveBeenCalled();
    });

    it("returns reload-required before marker work for a controller mismatch or retained reload", async () => {
      const entry = await readyRegistration();
      Object.defineProperty(navigator, "serviceWorker", { configurable: true, value: { controller: activeWorker() } });
      const { checkAppShellReadiness } = await import("../registerServiceWorker");
      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "reload-required", reason: "controller-mismatch" });

      Object.defineProperty(navigator, "serviceWorker", { configurable: true, value: { controller: entry.active } });
      const options = mocks.registerSW.mock.calls[0][0] as ServiceWorkerOptions;
      draftStore.setState({ role: "host", phase: "deckbuilding" });
      options.onNeedReload();
      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "reload-required", reason: "deferred-reload" });
      expect(fetch).not.toHaveBeenCalled();
      expect(mocks.markRemoteLoadOk).not.toHaveBeenCalled();
    });

    it("turns marker fetch, response, body, and subsequent misses into typed cache failures", async () => {
      await readyRegistration();
      const { checkAppShellReadiness } = await import("../registerServiceWorker");
      vi.mocked(fetch).mockImplementationOnce(() => { throw new Error("synchronous cache miss"); });
      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "not-ready", reason: "shell-cache-unavailable" });

      vi.mocked(fetch).mockRejectedValueOnce(new Error("cache miss"));
      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "not-ready", reason: "shell-cache-unavailable" });

      vi.mocked(fetch).mockResolvedValueOnce(jsonResponse({}, 503));
      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "not-ready", reason: "shell-cache-unavailable" });

      vi.mocked(fetch).mockResolvedValueOnce(malformedJsonResponse());
      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "not-ready", reason: "shell-cache-unavailable" });

      vi.mocked(fetch).mockResolvedValueOnce(jsonResponse([]));
      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "not-ready", reason: "shell-cache-unavailable" });

      vi.mocked(fetch)
        .mockResolvedValueOnce(jsonResponse({ build: __BUILD_HASH__ }))
        .mockResolvedValueOnce(jsonResponse({ build: "old" }));
      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "ready" });
      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "not-ready", reason: "shell-cache-unavailable" });
    });

    it("does not publish ready if the same active worker changes state while probing", async () => {
      const entry = await readyRegistration();
      const { checkAppShellReadiness } = await import("../registerServiceWorker");
      let resolveFetch!: (response: Response) => void;
      vi.mocked(fetch).mockImplementationOnce(() => new Promise<Response>((resolve) => { resolveFetch = resolve; }));

      const readiness = checkAppShellReadiness();
      Object.assign(entry.active!, { state: "activating" });
      resolveFetch(jsonResponse({ build: __BUILD_HASH__ }));

      await expect(readiness).resolves.toEqual({ status: "not-ready", reason: "update-in-progress" });
    });

    it("returns the current controller outcome when a pending probe rejects", async () => {
      const entry = await readyRegistration();
      const { checkAppShellReadiness } = await import("../registerServiceWorker");
      let rejectFetch!: (error: Error) => void;
      vi.mocked(fetch).mockImplementationOnce(() => new Promise<Response>((_resolve, reject) => { rejectFetch = reject; }));

      const readiness = checkAppShellReadiness();
      Object.defineProperty(navigator, "serviceWorker", { configurable: true, value: { controller: activeWorker() } });
      rejectFetch(new Error("cache unavailable"));

      await expect(readiness).resolves.toEqual({ status: "reload-required", reason: "controller-mismatch" });
      expect(entry.update).toHaveBeenCalledTimes(1);
    });

    it("reports an unsuccessful remote marker write without rejecting", async () => {
      await readyRegistration();
      const { checkAppShellReadiness } = await import("../registerServiceWorker");
      mocks.markRemoteLoadOk.mockResolvedValueOnce(false);
      vi.mocked(fetch).mockResolvedValueOnce(jsonResponse({ build: __BUILD_HASH__ }));

      await expect(checkAppShellReadiness()).resolves.toEqual({ status: "not-ready", reason: "remote-load-marker-unavailable" });
    });

    it("does not publish ready when the lifecycle changes while the marker is being written", async () => {
      await readyRegistration();
      const { checkAppShellReadiness, disposeServiceWorkerUpdater, registerServiceWorker } = await import("../registerServiceWorker");
      let resolveMarker!: (value: boolean) => void;
      mocks.markRemoteLoadOk.mockImplementationOnce(() => new Promise<boolean>((resolve) => { resolveMarker = resolve; }));
      vi.mocked(fetch).mockResolvedValueOnce(jsonResponse({ build: __BUILD_HASH__ }));

      const readiness = checkAppShellReadiness();
      await vi.waitFor(() => expect(mocks.markRemoteLoadOk).toHaveBeenCalledOnce());
      disposeServiceWorkerUpdater();
      registerServiceWorker();
      resolveMarker(true);

      await expect(readiness).resolves.toEqual({ status: "not-ready", reason: "lifecycle-changed" });
      expect(mocks.registerSW).toHaveBeenCalledTimes(2);
    });

    it("does not publish ready after controller or worker-state changes while the marker is pending", async () => {
      const entry = await readyRegistration();
      const { checkAppShellReadiness } = await import("../registerServiceWorker");
      let resolveMarker!: (value: boolean) => void;
      mocks.markRemoteLoadOk.mockImplementationOnce(() => new Promise<boolean>((resolve) => { resolveMarker = resolve; }));
      vi.mocked(fetch).mockResolvedValueOnce(jsonResponse({ build: __BUILD_HASH__ }));

      const controllerChange = checkAppShellReadiness();
      await vi.waitFor(() => expect(mocks.markRemoteLoadOk).toHaveBeenCalledOnce());
      Object.defineProperty(navigator, "serviceWorker", { configurable: true, value: { controller: activeWorker() } });
      resolveMarker(true);
      await expect(controllerChange).resolves.toEqual({ status: "reload-required", reason: "controller-mismatch" });

      Object.defineProperty(navigator, "serviceWorker", { configurable: true, value: { controller: entry.active } });
      let resolveSecondMarker!: (value: boolean) => void;
      mocks.markRemoteLoadOk.mockImplementationOnce(() => new Promise<boolean>((resolve) => { resolveSecondMarker = resolve; }));
      vi.mocked(fetch).mockResolvedValueOnce(jsonResponse({ build: __BUILD_HASH__ }));
      const stateChange = checkAppShellReadiness();
      await vi.waitFor(() => expect(mocks.markRemoteLoadOk).toHaveBeenCalledTimes(2));
      Object.assign(entry.active!, { state: "activating" });
      resolveSecondMarker(true);
      await expect(stateChange).resolves.toEqual({ status: "not-ready", reason: "update-in-progress" });
    });

    it("does not publish ready after re-registration replaces the lifecycle during a probe", async () => {
      await readyRegistration();
      const { checkAppShellReadiness, disposeServiceWorkerUpdater, registerServiceWorker } = await import("../registerServiceWorker");
      let resolveFetch!: (response: Response) => void;
      vi.mocked(fetch).mockImplementationOnce(() => new Promise<Response>((resolve) => { resolveFetch = resolve; }));

      const readiness = checkAppShellReadiness();
      disposeServiceWorkerUpdater();
      registerServiceWorker();
      resolveFetch(jsonResponse({ build: __BUILD_HASH__ }));

      await expect(readiness).resolves.toEqual({ status: "not-ready", reason: "lifecycle-changed" });
    });
  });
});
