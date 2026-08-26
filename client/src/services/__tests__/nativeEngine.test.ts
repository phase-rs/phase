import { beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock, isDesktopTauriMock, listenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  isDesktopTauriMock: vi.fn(),
  listenMock: vi.fn(),
}));

vi.mock("../platform", () => ({ isDesktopTauri: isDesktopTauriMock }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));

import {
  ensureNativeEngine,
  getNativeEngineProgress,
  subscribeNativeEngineProgress,
} from "../nativeEngine";

beforeEach(() => {
  vi.clearAllMocks();
  isDesktopTauriMock.mockReturnValue(false);
});

describe("native engine desktop boundary", () => {
  it("does not import or invoke native IPC on Android/iOS", async () => {
    await expect(ensureNativeEngine({ release: { version: "0.60.0" } })).rejects.toThrow(
      /desktop shell/,
    );
    expect(await getNativeEngineProgress()).toBeNull();
    expect(await subscribeNativeEngineProgress(vi.fn())).toEqual(expect.any(Function));
    expect(invokeMock).not.toHaveBeenCalled();
    expect(listenMock).not.toHaveBeenCalled();
  });

  it("retains desktop command and event reachability", async () => {
    const key = { release: { version: "0.60.0" } } as const;
    isDesktopTauriMock.mockReturnValue(true);
    invokeMock.mockResolvedValueOnce({ port: 9374 }).mockResolvedValueOnce(null);
    listenMock.mockResolvedValue(vi.fn());
    await expect(ensureNativeEngine(key)).resolves.toEqual({ port: 9374 });
    await expect(getNativeEngineProgress()).resolves.toBeNull();
    await subscribeNativeEngineProgress(vi.fn());
    expect(invokeMock).toHaveBeenCalledWith("ensure_native_engine", { key });
    expect(invokeMock).toHaveBeenCalledWith("native_engine_progress");
    expect(listenMock).toHaveBeenCalledWith("native-engine-progress", expect.any(Function));
  });
});
