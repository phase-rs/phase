import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  initializeFeeds: vi.fn(),
}));

vi.mock("../../services/feedService", () => ({ initializeFeeds: mocks.initializeFeeds }));

import { useFeedInitialization } from "../useFeedInitialization";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => cleanup());

describe("useFeedInitialization", () => {
  it("aborts the predecessor on mode transitions and unmount", () => {
    mocks.initializeFeeds.mockReturnValue(new Promise(() => undefined));
    const { rerender, unmount } = renderHook(
      ({ effectiveOffline }) => useFeedInitialization(effectiveOffline),
      { initialProps: { effectiveOffline: true } },
    );
    const offlineSignal = mocks.initializeFeeds.mock.calls[0][0].signal as AbortSignal;

    act(() => rerender({ effectiveOffline: false }));
    const onlineSignal = mocks.initializeFeeds.mock.calls[1][0].signal as AbortSignal;
    expect(mocks.initializeFeeds).toHaveBeenNthCalledWith(
      1,
      expect.objectContaining({ allowRefresh: false, signal: offlineSignal }),
    );
    expect(mocks.initializeFeeds).toHaveBeenNthCalledWith(
      2,
      expect.objectContaining({ allowRefresh: true, signal: onlineSignal }),
    );
    expect(offlineSignal.aborted).toBe(true);
    expect(onlineSignal).not.toBe(offlineSignal);
    expect(onlineSignal.aborted).toBe(false);

    unmount();
    expect(onlineSignal.aborted).toBe(true);
  });

  it("keeps AbortError and superseded failures silent", async () => {
    const first = deferred<void>();
    const second = deferred<void>();
    mocks.initializeFeeds
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const { result, rerender } = renderHook(
      ({ effectiveOffline }) => useFeedInitialization(effectiveOffline),
      { initialProps: { effectiveOffline: true } },
    );

    act(() => rerender({ effectiveOffline: false }));
    await act(async () => {
      first.reject(new Error("superseded"));
      second.reject(new DOMException("aborted", "AbortError"));
    });

    expect(error).not.toHaveBeenCalled();
    expect(result.current).toBe(false);
  });

  it("reports the current non-abort failure once", async () => {
    const pending = deferred<void>();
    mocks.initializeFeeds.mockReturnValueOnce(pending.promise);
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    renderHook(() => useFeedInitialization(false));

    pending.reject(new Error("current failure"));
    await Promise.resolve();

    expect(error).toHaveBeenCalledTimes(1);
    expect(error).toHaveBeenCalledWith("Feed initialization failed:", expect.any(Error));
  });

  it("publishes readiness only for the current settled mode", async () => {
    const offline = deferred<void>();
    const online = deferred<void>();
    mocks.initializeFeeds.mockReturnValueOnce(offline.promise).mockReturnValueOnce(online.promise);
    const { result, rerender } = renderHook(
      ({ effectiveOffline }) => useFeedInitialization(effectiveOffline),
      { initialProps: { effectiveOffline: true } },
    );
    expect(result.current).toBe(false);

    act(() => rerender({ effectiveOffline: false }));
    expect(result.current).toBe(false);
    await act(async () => { offline.resolve(); });
    expect(result.current).toBe(false);
    await act(async () => { online.resolve(); });
    expect(result.current).toBe(true);
  });

  it("treats a handled current failure as settled without publishing an aborted generation", async () => {
    const offline = deferred<void>();
    const online = deferred<void>();
    mocks.initializeFeeds.mockReturnValueOnce(offline.promise).mockReturnValueOnce(online.promise);
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const { result, rerender } = renderHook(
      ({ effectiveOffline }) => useFeedInitialization(effectiveOffline),
      { initialProps: { effectiveOffline: true } },
    );
    act(() => rerender({ effectiveOffline: false }));
    await act(async () => { offline.reject(new Error("stale")); });
    expect(result.current).toBe(false);
    await act(async () => { online.reject(new Error("current")); });
    expect(result.current).toBe(true);
    expect(error).toHaveBeenCalledTimes(1);
  });
});
