import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

describe("useFixedVisualImage", () => {
  beforeEach(() => {
    vi.resetModules();
    vi.restoreAllMocks();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("keeps installed card backs local while offline and exposes their known remote fallback online", async () => {
    const resolve = vi.fn(async ({ allowRemote }: { allowRemote: boolean }) => ({
      revision: "0",
      sources: [
        {
          kind: "installed" as const,
          src: "installed-back",
          assetKey: "asset:v1:card_back:W10",
          packId: "core",
          catalogRoot: "a".repeat(64),
        },
        ...(allowRemote ? [{ kind: "remote" as const, src: "remote-back" }] : []),
        { kind: "fallback" as const, src: null },
      ],
    }));
    vi.doMock("../../services/visualPacks/repository.ts", () => ({
      visualPackRepository: { currentRevision: () => "0", subscribe: () => () => {}, resolve },
    }));

    const { useConnectivityStore } = await import("../../stores/connectivityStore.ts");
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    const { cardBackCandidate } = await import("../../services/visualPacks/candidateKeys.ts");
    const { useFixedVisualImage } = await import("../useFixedVisualImage.ts");
    const { result } = renderHook(() => useFixedVisualImage(cardBackCandidate(), "remote-back"));

    await waitFor(() => expect(result.current.src).toBe("installed-back"));
    expect(resolve).toHaveBeenLastCalledWith(expect.objectContaining({ allowRemote: true }));
    act(() => result.current.advanceFailedSource("installed-back"));
    expect(result.current.src).toBe("remote-back");

    act(() => useConnectivityStore.getState().setForcedOffline(true));
    await waitFor(() => expect(result.current.src).toBe("installed-back"));
    expect(resolve).toHaveBeenLastCalledWith(expect.objectContaining({ allowRemote: false }));
    act(() => result.current.advanceFailedSource("installed-back"));
    expect(result.current.src).toBeNull();

    act(() => useConnectivityStore.getState().setForcedOffline(false));
    await waitFor(() => expect(result.current.src).toBe("installed-back"));
    expect(resolve).toHaveBeenLastCalledWith(expect.objectContaining({ allowRemote: true }));
  });

  it("settles a missing fixed candidate locally offline and uses its known remote online", async () => {
    const resolve = vi.fn(async ({ allowRemote }: { allowRemote: boolean }) => ({
      revision: "0",
      sources: allowRemote
        ? [{ kind: "remote" as const, src: "remote-symbol" }, { kind: "fallback" as const, src: null }]
        : [{ kind: "fallback" as const, src: null }],
    }));
    vi.doMock("../../services/visualPacks/repository.ts", () => ({
      visualPackRepository: { currentRevision: () => "0", subscribe: () => () => {}, resolve },
    }));

    const { useConnectivityStore } = await import("../../stores/connectivityStore.ts");
    useConnectivityStore.setState({ forcedOffline: true, browserOnline: true });
    const { manaSymbolCandidate } = await import("../../services/visualPacks/candidateKeys.ts");
    const { useFixedVisualImage } = await import("../useFixedVisualImage.ts");
    const { result } = renderHook(() => useFixedVisualImage(manaSymbolCandidate("{W}"), "remote-symbol"));

    await waitFor(() => expect(result.current.isLoading).toBe(false));
    expect(result.current.src).toBeNull();
    expect(resolve).toHaveBeenLastCalledWith(expect.objectContaining({ allowRemote: false }));

    act(() => useConnectivityStore.getState().setForcedOffline(false));
    await waitFor(() => expect(result.current.src).toBe("remote-symbol"));
    expect(resolve).toHaveBeenLastCalledWith(expect.objectContaining({ allowRemote: true }));
  });

  it("drops an outdated deferred online result after a policy transition", async () => {
    type Resolution = { revision: string; sources: Array<{ kind: "remote" | "fallback"; src: string | null }> };
    const pending: Array<(result: Resolution) => void> = [];
    const resolve = vi.fn(() => new Promise<Resolution>((complete) => {
      pending.push(complete);
    }));
    vi.doMock("../../services/visualPacks/repository.ts", () => ({
      visualPackRepository: { currentRevision: () => "0", subscribe: () => () => {}, resolve },
    }));

    const { useConnectivityStore } = await import("../../stores/connectivityStore.ts");
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    const { cardBackCandidate } = await import("../../services/visualPacks/candidateKeys.ts");
    const { useFixedVisualImage } = await import("../useFixedVisualImage.ts");
    const { result } = renderHook(() => useFixedVisualImage(cardBackCandidate(), "remote-back"));

    await waitFor(() => expect(resolve).toHaveBeenCalledTimes(1));
    act(() => useConnectivityStore.getState().setForcedOffline(true));
    await waitFor(() => expect(resolve).toHaveBeenCalledTimes(2));
    await act(async () => pending[0]({
      revision: "0",
      sources: [{ kind: "remote", src: "stale-remote" }, { kind: "fallback", src: null }],
    }));
    expect(result.current.src).toBeNull();
    expect(result.current.isLoading).toBe(true);
    await act(async () => pending[1]({
      revision: "0",
      sources: [{ kind: "fallback", src: null }],
    }));
    await waitFor(() => expect(result.current.isLoading).toBe(false));
    expect(result.current.src).toBeNull();

    act(() => useConnectivityStore.getState().setForcedOffline(false));
    await waitFor(() => expect(resolve).toHaveBeenCalledTimes(3));
    await act(async () => pending[2]({
      revision: "0",
      sources: [{ kind: "remote", src: "fresh-remote" }, { kind: "fallback", src: null }],
    }));
    await waitFor(() => expect(result.current.src).toBe("fresh-remote"));
  });
});
