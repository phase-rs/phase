import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock, isDesktopTauriMock, listenMock, effectiveOfflineMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  isDesktopTauriMock: vi.fn(),
  listenMock: vi.fn(),
  effectiveOfflineMock: vi.fn(),
}));

vi.mock("../platform", () => ({ isDesktopTauri: isDesktopTauriMock }));
vi.mock("../../stores/connectivityStore", () => ({
  getEffectiveOffline: effectiveOfflineMock,
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));

async function nativeEngine() {
  await import("@tauri-apps/api/core");
  return import("../nativeEngine");
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.resetModules();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
  isDesktopTauriMock.mockReturnValue(false);
  effectiveOfflineMock.mockReturnValue(false);
});

describe("native engine desktop boundary", () => {
  it("does not import or invoke native IPC on Android/iOS", async () => {
    const {
      ensureNativeEngine,
      getNativeEngineProgress,
      prepareNativeEngineForOffline,
      subscribeNativeEngineProgress,
    } = await nativeEngine();
    await expect(ensureNativeEngine({ release: { version: "0.60.0" } })).rejects.toThrow(
      /desktop shell/,
    );
    await expect(
      prepareNativeEngineForOffline({ release: { version: "0.60.0" } }),
    ).rejects.toThrow(/desktop shell/);
    expect(await getNativeEngineProgress()).toBeNull();
    expect(await subscribeNativeEngineProgress(vi.fn())).toEqual(expect.any(Function));
    expect(invokeMock).not.toHaveBeenCalled();
    expect(listenMock).not.toHaveBeenCalled();
  });

  it("maps only trusted stamped origins to native-engine keys", async () => {
    const { canAttemptNativeEngine, nativeEngineKeyForCurrentOrigin } = await nativeEngine();
    vi.stubGlobal("window", { location: { origin: "https://phase-rs.dev" } });
    expect(nativeEngineKeyForCurrentOrigin()).toEqual({
      release: { version: expect.any(String) },
    });
    isDesktopTauriMock.mockReturnValue(true);
    expect(canAttemptNativeEngine(true)).toBe(true);

    vi.stubGlobal("window", { location: { origin: "https://preview.phase-rs.dev" } });
    // Test builds deliberately omit the preview fingerprint, so preview stays
    // on WASM until a stamped release supplies an exact artifact key.
    vi.stubGlobal("__ENGINE_FINGERPRINT__", undefined);
    expect(nativeEngineKeyForCurrentOrigin()).toBeNull();
    vi.stubGlobal("__ENGINE_FINGERPRINT__", "0123456789abcdef");
    expect(nativeEngineKeyForCurrentOrigin()).toEqual({
      preview: { fingerprint: "0123456789abcdef" },
    });

    vi.stubGlobal("window", { location: { origin: "https://example.test" } });
    expect(nativeEngineKeyForCurrentOrigin()).toBeNull();
    expect(canAttemptNativeEngine(true)).toBe(false);
  });

  it("keeps online startup on the legacy omitted-intent payload without a probe", async () => {
    const { ensureNativeEngine } = await nativeEngine();
    const key = { release: { version: "0.60.0" } } as const;
    isDesktopTauriMock.mockReturnValue(true);
    invokeMock.mockResolvedValue({ port: 9374 });

    await expect(ensureNativeEngine(key)).resolves.toEqual({ port: 9374 });

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("ensure_native_engine", { key });
  });

  it("uses the effective offline policy and probes before explicit intents", async () => {
    const { ensureNativeEngine } = await nativeEngine();
    const key = { preview: { fingerprint: "0123456789abcdef" } } as const;
    isDesktopTauriMock.mockReturnValue(true);
    effectiveOfflineMock.mockReturnValue(true);
    invokeMock
      .mockResolvedValueOnce({ intent_contract: 1 })
      .mockResolvedValueOnce({ port: 9374 });

    await expect(ensureNativeEngine(key)).resolves.toEqual({ port: 9374 });

    expect(invokeMock).toHaveBeenNthCalledWith(1, "native_engine_capabilities");
    expect(invokeMock).toHaveBeenNthCalledWith(2, "ensure_native_engine", {
      key,
      intent: "start_offline",
    });
  });

  it("keeps failed capability results fail-closed and never invokes the engine", async () => {
    const { prepareNativeEngineForOffline } = await nativeEngine();
    isDesktopTauriMock.mockReturnValue(true);
    invokeMock.mockRejectedValue(new Error("unknown command"));

    await expect(
      prepareNativeEngineForOffline({ release: { version: "0.60.0" } }),
    ).rejects.toThrow("unknown command");

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("native_engine_capabilities");
  });

  it("settles provisioning after a capability rejection", async () => {
    const { prepareNativeEngineForOffline, useNativeEngineProvisioning } = await nativeEngine();
    isDesktopTauriMock.mockReturnValue(true);
    invokeMock.mockRejectedValue(new Error("unknown command"));
    const { result } = renderHook(() => useNativeEngineProvisioning());

    await expect(
      prepareNativeEngineForOffline({ release: { version: "0.60.0" } }),
    ).rejects.toThrow("unknown command");

    expect(result.current).toBe(false);
  });

  it("settles provisioning after an engine rejection", async () => {
    const { prepareNativeEngineForOffline, useNativeEngineProvisioning } = await nativeEngine();
    isDesktopTauriMock.mockReturnValue(true);
    invokeMock
      .mockResolvedValueOnce({ intent_contract: 1 })
      .mockRejectedValueOnce(new Error("provisioning failed"));
    const { result } = renderHook(() => useNativeEngineProvisioning());

    await expect(
      prepareNativeEngineForOffline({ release: { version: "0.60.0" } }),
    ).rejects.toThrow("provisioning failed");

    expect(result.current).toBe(false);
  });

  it.each([
    ["missing", undefined],
    ["malformed", {}],
    ["unsupported", { intent_contract: 2 }],
  ])("rejects a %s capability result before native-engine IPC", async (_case, capability) => {
    const { prepareNativeEngineForOffline } = await nativeEngine();
    isDesktopTauriMock.mockReturnValue(true);
    invokeMock.mockResolvedValue(capability);

    await expect(
      prepareNativeEngineForOffline({ release: { version: "0.60.0" } }),
    ).rejects.toThrow(/does not support/);

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("native_engine_capabilities");
  });

  it("shares the capability probe across concurrent offline and preparation calls", async () => {
    const { ensureNativeEngine, prepareNativeEngineForOffline } = await nativeEngine();
    const key = { release: { version: "0.60.0" } } as const;
    isDesktopTauriMock.mockReturnValue(true);
    effectiveOfflineMock.mockReturnValue(true);
    invokeMock
      .mockResolvedValueOnce({ intent_contract: 1 })
      .mockResolvedValue({ port: 9374 });

    await expect(Promise.all([ensureNativeEngine(key), prepareNativeEngineForOffline(key)])).resolves.toEqual([
      { port: 9374 },
      { port: 9374 },
    ]);

    expect(invokeMock).toHaveBeenCalledTimes(3);
    expect(invokeMock).toHaveBeenCalledWith("native_engine_capabilities");
    expect(invokeMock).toHaveBeenCalledWith("ensure_native_engine", {
      key,
      intent: "start_offline",
    });
    expect(invokeMock).toHaveBeenCalledWith("ensure_native_engine", {
      key,
      intent: "prepare_for_offline",
    });
  });

  it("reuses a settled capability probe for later preparation", async () => {
    const { prepareNativeEngineForOffline } = await nativeEngine();
    const key = { release: { version: "0.60.0" } } as const;
    isDesktopTauriMock.mockReturnValue(true);
    invokeMock
      .mockResolvedValueOnce({ intent_contract: 1 })
      .mockResolvedValue({ port: 9374 });

    await prepareNativeEngineForOffline(key);
    await prepareNativeEngineForOffline(key);

    expect(invokeMock).toHaveBeenCalledTimes(3);
    expect(invokeMock.mock.calls.filter(([command]) => command === "native_engine_capabilities"))
      .toHaveLength(1);
  });

  it("keeps provisioning active across the capability probe and engine IPC", async () => {
    const { prepareNativeEngineForOffline, useNativeEngineProvisioning } = await nativeEngine();
    const capability = deferred<{ intent_contract: 1 }>();
    const engine = deferred<{ port: number }>();
    isDesktopTauriMock.mockReturnValue(true);
    invokeMock.mockReturnValueOnce(capability.promise).mockReturnValueOnce(engine.promise);
    const { result } = renderHook(() => useNativeEngineProvisioning());

    const pending = prepareNativeEngineForOffline({ release: { version: "0.60.0" } });
    await act(async () => {
      await Promise.resolve();
    });
    expect(result.current).toBe(true);
    await act(async () => capability.resolve({ intent_contract: 1 }));
    expect(result.current).toBe(true);
    await act(async () => engine.resolve({ port: 9374 }));
    await expect(pending).resolves.toEqual({ port: 9374 });
    expect(result.current).toBe(false);
  });

  it("retains desktop progress and event reachability", async () => {
    const { getNativeEngineProgress, subscribeNativeEngineProgress } = await nativeEngine();
    isDesktopTauriMock.mockReturnValue(true);
    invokeMock.mockResolvedValueOnce(null);
    listenMock.mockResolvedValue(vi.fn());

    await expect(getNativeEngineProgress()).resolves.toBeNull();
    await subscribeNativeEngineProgress(vi.fn());
    expect(invokeMock).toHaveBeenCalledWith("native_engine_progress");
    expect(listenMock).toHaveBeenCalledWith("native-engine-progress", expect.any(Function));
  });
});
