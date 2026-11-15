import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock, isTauriMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  isTauriMock: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("../platform", () => ({ isTauri: isTauriMock }));

// The service caches its verdict promise at module scope — fresh module per
// test (changelog.test.ts idiom).
async function freshAudioDeviceSafe() {
  vi.resetModules();
  const { audioDeviceSafe } = await import("../audioHealth");
  return audioDeviceSafe;
}

describe("audioDeviceSafe", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    isTauriMock.mockReturnValue(true);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("a healthy verdict is safe", async () => {
    invokeMock.mockResolvedValue("healthy");
    const audioDeviceSafe = await freshAudioDeviceSafe();
    await expect(audioDeviceSafe()).resolves.toBe(true);
    expect(invokeMock).toHaveBeenCalledWith("audio_boot_health");
  });

  it("a wedged verdict is unsafe — the one gating outcome", async () => {
    invokeMock.mockResolvedValue("wedged");
    const audioDeviceSafe = await freshAudioDeviceSafe();
    await expect(audioDeviceSafe()).resolves.toBe(false);
  });

  it("an unknown verdict fails open", async () => {
    invokeMock.mockResolvedValue("unknown");
    const audioDeviceSafe = await freshAudioDeviceSafe();
    await expect(audioDeviceSafe()).resolves.toBe(true);
  });

  it("non-Tauri hosts never invoke and are safe", async () => {
    isTauriMock.mockReturnValue(false);
    const audioDeviceSafe = await freshAudioDeviceSafe();
    await expect(audioDeviceSafe()).resolves.toBe(true);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("a rejecting invoke (old shell without the command) fails open", async () => {
    invokeMock.mockRejectedValue(new Error("unknown command audio_boot_health"));
    const audioDeviceSafe = await freshAudioDeviceSafe();
    await expect(audioDeviceSafe()).resolves.toBe(true);
  });

  it("the verdict is cached — one invoke across repeated calls", async () => {
    invokeMock.mockResolvedValue("healthy");
    const audioDeviceSafe = await freshAudioDeviceSafe();
    await audioDeviceSafe();
    await audioDeviceSafe();
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("a never-settling invoke fails open after the 6s cap", async () => {
    vi.useFakeTimers();
    invokeMock.mockReturnValue(new Promise(() => {}));
    const audioDeviceSafe = await freshAudioDeviceSafe();
    const verdict = audioDeviceSafe();
    await vi.advanceTimersByTimeAsync(6001);
    await expect(verdict).resolves.toBe(true);
  });
});
