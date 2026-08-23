import { beforeEach, describe, expect, it, vi } from "vitest";

// Claim boundary: `audioDeviceSafe` is mocked here, so this file proves the
// DISABLE WIRING (verdict → disable → silent, completed boot) and its
// ordering. The service's own seam (isTauri, invoke, timeout race) is proven
// in services/__tests__/audioHealth.test.ts.
const { audioDeviceSafeMock } = vi.hoisted(() => ({
  audioDeviceSafeMock: vi.fn(),
}));
vi.mock("../../services/audioHealth", () => ({ audioDeviceSafe: audioDeviceSafeMock }));

const audioContextSpy = vi.fn().mockImplementation(function () {
  return {
    createGain: vi.fn().mockImplementation(() => ({
      gain: {
        value: 1,
        cancelScheduledValues: vi.fn(),
        setValueAtTime: vi.fn(),
        linearRampToValueAtTime: vi.fn(),
      },
      connect: vi.fn(),
    })),
    decodeAudioData: vi.fn().mockResolvedValue({}),
    close: vi.fn(),
    destination: {},
    currentTime: 0,
  };
});
vi.stubGlobal("AudioContext", audioContextSpy);

vi.stubGlobal(
  "fetch",
  vi.fn().mockResolvedValue({ arrayBuffer: () => Promise.resolve(new ArrayBuffer(8)) }),
);

// Avoid IndexedDB in happy-dom.
vi.mock("../../audio/audioCache", () => ({
  fetchWithCache: vi.fn().mockResolvedValue(new ArrayBuffer(8)),
  getCachedManifest: vi.fn().mockResolvedValue(null),
  cacheThemeManifest: vi.fn().mockResolvedValue(undefined),
  clearThemeCache: vi.fn().mockResolvedValue(undefined),
}));

// preloadAssets caches its promise and audioManager latches isWarmedUp, both
// at module scope — fresh module graph per test (changelog.test.ts idiom).
async function freshBoot() {
  vi.resetModules();
  const preload = await import("../preloadAssets");
  const { audioManager } = await import("../../audio/AudioManager");
  const { useAudioHealthStore } = await import("../../stores/audioHealthStore");
  return { ...preload, audioManager, useAudioHealthStore };
}

describe("ensurePreload audio gate", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("wedged verdict: boot completes silent, disable precedes gesture listeners", async () => {
    audioDeviceSafeMock.mockResolvedValue(false);
    const addListenerSpy = vi.spyOn(document, "addEventListener");
    const { ensurePreload, subscribePreload, audioManager, useAudioHealthStore } =
      await freshBoot();
    const disableSpy = vi.spyOn(audioManager, "disable");

    const percents: number[] = [];
    const unsub = subscribePreload((p) => percents.push(p.percent));
    await ensurePreload();
    unsub();

    // Silent: the device was never opened…
    expect(audioContextSpy).not.toHaveBeenCalled();
    // …but the boot still completed and the splash can dismiss.
    expect(percents).toContain(100);
    expect(useAudioHealthStore.getState().deviceBlocked).toBe(true);

    // Ordering, not mere co-occurrence: disable() must run before ANY gesture
    // listener exists, else a click during boot could still reach warmUp().
    expect(disableSpy).toHaveBeenCalledOnce();
    const disableOrder = disableSpy.mock.invocationCallOrder[0];
    const listenerOrders = addListenerSpy.mock.invocationCallOrder;
    expect(listenerOrders.length).toBeGreaterThan(0);
    for (const order of listenerOrders) {
      expect(disableOrder).toBeLessThan(order);
    }
    addListenerSpy.mockRestore();
  });

  // Control arm: flipping only the verdict flips the device-open observable,
  // so the wedged test's not-called assertion is discriminating.
  it("healthy verdict: warmUp opens the device and nothing is blocked", async () => {
    audioDeviceSafeMock.mockResolvedValue(true);
    const { ensurePreload, subscribePreload, useAudioHealthStore } = await freshBoot();

    const percents: number[] = [];
    const unsub = subscribePreload((p) => percents.push(p.percent));
    await ensurePreload();
    unsub();

    expect(audioContextSpy).toHaveBeenCalledOnce();
    expect(percents).toContain(100);
    expect(useAudioHealthStore.getState().deviceBlocked).toBe(false);
  });

  it("progress starts moving before the verdict resolves (splash is never 0%-frozen)", async () => {
    let resolveVerdict!: (safe: boolean) => void;
    audioDeviceSafeMock.mockReturnValue(
      new Promise<boolean>((resolve) => {
        resolveVerdict = resolve;
      }),
    );
    const { ensurePreload, subscribePreload } = await freshBoot();

    const percents: number[] = [];
    const unsub = subscribePreload((p) => percents.push(p.percent));
    const done = ensurePreload();

    expect(percents).toContain(10);
    expect(percents).not.toContain(100);

    resolveVerdict(true);
    await done;
    unsub();
    expect(percents).toContain(100);
  });
});
