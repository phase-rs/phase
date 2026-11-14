import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const STORAGE_KEY = "phase-connectivity-v1";

type ConnectivityModule = typeof import("../connectivityStore");

let store: ConnectivityModule | undefined;
let navigatorOnLineDescriptor: PropertyDescriptor | undefined;

function setNavigatorOnline(value: boolean | undefined): void {
  Object.defineProperty(navigator, "onLine", {
    configurable: true,
    value,
  });
}

async function loadStore(): Promise<ConnectivityModule> {
  vi.resetModules();
  store = await import("../connectivityStore");
  return store;
}

beforeEach(() => {
  localStorage.clear();
  navigatorOnLineDescriptor = Object.getOwnPropertyDescriptor(navigator, "onLine");
  setNavigatorOnline(true);
});

afterEach(() => {
  store?.disposeConnectivity();
  store = undefined;
  if (navigatorOnLineDescriptor) {
    Object.defineProperty(navigator, "onLine", navigatorOnLineDescriptor);
  } else {
    Reflect.deleteProperty(navigator, "onLine");
  }
  vi.restoreAllMocks();
});

describe("connectivityStore", () => {
  it("defers persisted forced-offline preference until explicit initialization", async () => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ state: { forcedOffline: true }, version: 1 }));

    const connectivity = await loadStore();
    expect(connectivity.useConnectivityStore.getState().forcedOffline).toBe(false);
    expect(connectivity.useConnectivityStore.persist.hasHydrated()).toBe(false);

    await connectivity.initializeConnectivity();

    expect(connectivity.useConnectivityStore.getState().forcedOffline).toBe(true);
    expect(connectivity.getEffectiveOffline()).toBe(true);
    expect(connectivity.useConnectivityStore.persist.hasHydrated()).toBe(true);
  });

  it("persists only the forced preference and allows it to be cleared", async () => {
    const connectivity = await loadStore();
    await connectivity.initializeConnectivity();

    connectivity.useConnectivityStore.getState().setForcedOffline(true);
    expect(connectivity.getEffectiveOffline()).toBe(true);
    expect(JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}")).toEqual({
      state: { forcedOffline: true },
      version: 1,
    });

    connectivity.useConnectivityStore.getState().setForcedOffline(false);
    expect(connectivity.getEffectiveOffline()).toBe(false);
    expect(JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}")).toEqual({
      state: { forcedOffline: false },
      version: 1,
    });
  });

  it("follows browser connectivity transitions", async () => {
    const connectivity = await loadStore();
    await connectivity.initializeConnectivity();
    const changes: boolean[] = [];
    const unsubscribe = connectivity.subscribeEffectiveOffline((offline) => changes.push(offline));

    setNavigatorOnline(false);
    window.dispatchEvent(new Event("offline"));
    setNavigatorOnline(true);
    window.dispatchEvent(new Event("online"));

    expect(changes).toEqual([true, false]);
    expect(connectivity.getEffectiveOffline()).toBe(false);
    unsubscribe();
  });

  it("does not notify for masked or repeated browser events and notifies once when forced mode clears", async () => {
    const connectivity = await loadStore();
    await connectivity.initializeConnectivity();
    connectivity.useConnectivityStore.getState().setForcedOffline(true);
    const changes: boolean[] = [];
    const unsubscribe = connectivity.subscribeEffectiveOffline((offline) => changes.push(offline));

    window.dispatchEvent(new Event("online"));
    window.dispatchEvent(new Event("online"));
    setNavigatorOnline(false);
    window.dispatchEvent(new Event("offline"));
    setNavigatorOnline(true);
    window.dispatchEvent(new Event("online"));
    expect(changes).toEqual([]);

    connectivity.useConnectivityStore.getState().setForcedOffline(false);
    connectivity.useConnectivityStore.getState().setForcedOffline(false);
    expect(changes).toEqual([false]);
    unsubscribe();
  });

  it("installs one listener pair, cleans it up, and can initialize again", async () => {
    const addEventListener = vi.spyOn(window, "addEventListener");
    const removeEventListener = vi.spyOn(window, "removeEventListener");
    const connectivity = await loadStore();

    await connectivity.initializeConnectivity();
    await connectivity.initializeConnectivity();
    expect(addEventListener.mock.calls.filter(([event]) => event === "online")).toHaveLength(1);
    expect(addEventListener.mock.calls.filter(([event]) => event === "offline")).toHaveLength(1);

    connectivity.disposeConnectivity();
    expect(removeEventListener.mock.calls.filter(([event]) => event === "online")).toHaveLength(1);
    expect(removeEventListener.mock.calls.filter(([event]) => event === "offline")).toHaveLength(1);

    await connectivity.initializeConnectivity();
    expect(addEventListener.mock.calls.filter(([event]) => event === "online")).toHaveLength(2);
    expect(addEventListener.mock.calls.filter(([event]) => event === "offline")).toHaveLength(2);
  });

  it("ignores an HMR-style disposal that races hydration", async () => {
    const addEventListener = vi.spyOn(window, "addEventListener");
    const connectivity = await loadStore();
    let releaseRehydrate!: () => void;
    const rehydrating = new Promise<void>((resolve) => { releaseRehydrate = resolve; });
    vi.spyOn(connectivity.useConnectivityStore.persist, "rehydrate")
      .mockImplementationOnce(async () => rehydrating)
      .mockResolvedValue(undefined);

    const firstInitialization = connectivity.initializeConnectivity();
    await vi.waitFor(() => expect(connectivity.useConnectivityStore.persist.rehydrate).toHaveBeenCalledOnce());
    connectivity.disposeConnectivity();
    releaseRehydrate();
    await firstInitialization;
    expect(addEventListener).not.toHaveBeenCalledWith("online", expect.any(Function));

    await connectivity.initializeConnectivity();
    expect(addEventListener).toHaveBeenCalledWith("online", expect.any(Function));
  });

  it("defaults to online when navigator does not expose onLine", async () => {
    setNavigatorOnline(undefined);
    const connectivity = await loadStore();

    expect(connectivity.useConnectivityStore.getState().browserOnline).toBe(true);
    await connectivity.initializeConnectivity();
    expect(connectivity.getEffectiveOffline()).toBe(false);
  });
});
