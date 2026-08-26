import { beforeEach, describe, expect, it, vi } from "vitest";

// Separate file from AudioManager.test.ts on purpose: `disable()` is
// deliberately sticky on the module singleton, so these tests get a fresh
// module per test (changelog.test.ts idiom) instead of sharing the suite's
// singleton and leaking the flag into its assertions.

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
    createBufferSource: vi.fn(),
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
vi.mock("../audioCache", () => ({
  fetchWithCache: vi.fn().mockResolvedValue(new ArrayBuffer(8)),
  getCachedManifest: vi.fn().mockResolvedValue(null),
  cacheThemeManifest: vi.fn().mockResolvedValue(undefined),
  clearThemeCache: vi.fn().mockResolvedValue(undefined),
}));

async function freshAudioManager() {
  vi.resetModules();
  const { audioManager } = await import("../AudioManager");
  return audioManager;
}

describe("AudioManager.disable", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  // Disabled tests arm device-open first so they discriminate the disable()
  // gate specifically — unarmed, warmUp would no-op anyway.
  it("warmUp is a device-free no-op while disabled", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.disable();
    audioManager.warmUp();
    expect(audioContextSpy).not.toHaveBeenCalled();
    expect(audioManager.isDisabled).toBe(true);
  });

  // Control arm: proves the spy would fire if the gates were broken, so the
  // not-called assertions above and below are non-vacuous.
  it("armed and not disabled, the same fresh module DOES open the device", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.warmUp();
    expect(audioContextSpy).toHaveBeenCalledOnce();
  });

  // Pre-verdict callers (e.g. Tab+Enter reaching VolumeControl through the
  // splash overlay) must not open the device before ensurePreload() arms it.
  it("before armDeviceOpen, warmUp/restart/ensurePlayback are device-free", async () => {
    const audioManager = await freshAudioManager();
    audioManager.warmUp();
    await audioManager.restart();
    audioManager.ensurePlayback();
    expect(audioContextSpy).not.toHaveBeenCalled();
    expect(audioManager.isDisabled).toBe(false);
  });

  it("restart stays device-free while disabled (dispose + warmUp cycle)", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.disable();
    await audioManager.restart();
    expect(audioContextSpy).not.toHaveBeenCalled();
  });

  it("ensurePlayback stays device-free while disabled", async () => {
    const audioManager = await freshAudioManager();
    audioManager.armDeviceOpen();
    audioManager.disable();
    audioManager.ensurePlayback();
    expect(audioContextSpy).not.toHaveBeenCalled();
  });

  it("preloadSfx resolves without a context while disabled", async () => {
    const audioManager = await freshAudioManager();
    audioManager.disable();
    await expect(audioManager.preloadSfx()).resolves.toBeUndefined();
  });

  it("diagnostics reports the disabled state", async () => {
    const audioManager = await freshAudioManager();
    audioManager.disable();
    expect(audioManager.diagnostics()).toContain("ctx=disabled");
  });
});
