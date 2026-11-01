import { act, cleanup, renderHook } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore } from "../../stores/preferencesStore";
import { usePriorityAttentionStore } from "../../stores/priorityAttentionStore";
import { usePriorityChime } from "../usePriorityChime";

const playCue = vi.hoisted(() => vi.fn());

vi.mock("../../audio/AudioManager", () => ({
  audioManager: { playCue },
}));

beforeEach(() => {
  playCue.mockClear();
  usePriorityAttentionStore.setState({ cue: "none", chimeCount: 4 });
  usePreferencesStore.setState({ priorityAlertSound: true });
});

afterEach(() => {
  cleanup();
  usePriorityAttentionStore.setState({ cue: "none", chimeCount: 0 });
  usePreferencesStore.setState({ priorityAlertSound: true });
});

describe("usePriorityChime", () => {
  it("ignores the initial count and plays once for each new chime", () => {
    const { rerender } = renderHook(() => usePriorityChime(), { wrapper: StrictMode });
    expect(playCue).not.toHaveBeenCalled();

    act(() => usePriorityAttentionStore.setState({ cue: "announced", chimeCount: 5 }));
    expect(playCue).toHaveBeenCalledExactlyOnceWith("priority", expect.any(AbortSignal));

    act(() => usePriorityAttentionStore.setState({ cue: "reminded" }));
    rerender();
    expect(playCue).toHaveBeenCalledTimes(1);

    act(() => usePriorityAttentionStore.setState({ chimeCount: 6 }));
    expect(playCue).toHaveBeenCalledTimes(2);
  });

  it("consumes muted chimes without replaying them when the preference is enabled", () => {
    usePreferencesStore.setState({ priorityAlertSound: false });
    renderHook(() => usePriorityChime());

    act(() => usePriorityAttentionStore.setState({ cue: "announced", chimeCount: 5 }));
    expect(playCue).not.toHaveBeenCalled();
    act(() => usePreferencesStore.setState({ priorityAlertSound: true }));
    expect(playCue).not.toHaveBeenCalled();

    act(() => usePriorityAttentionStore.setState({ chimeCount: 6 }));
    expect(playCue).toHaveBeenCalledExactlyOnceWith("priority", expect.any(AbortSignal));
  });

  it("does not replay a chime after unmount and remount", () => {
    const first = renderHook(() => usePriorityChime());
    act(() => usePriorityAttentionStore.setState({ chimeCount: 5 }));
    expect(playCue).toHaveBeenCalledTimes(1);
    first.unmount();

    renderHook(() => usePriorityChime());
    expect(playCue).toHaveBeenCalledTimes(1);
    act(() => usePriorityAttentionStore.setState({ chimeCount: 6 }));
    expect(playCue).toHaveBeenCalledTimes(2);
  });

  it("treats a counter reset as a new baseline, not a chime", () => {
    renderHook(() => usePriorityChime());
    act(() => usePriorityAttentionStore.setState({ chimeCount: 0 }));
    expect(playCue).not.toHaveBeenCalled();

    act(() => usePriorityAttentionStore.setState({ chimeCount: 1 }));
    expect(playCue).toHaveBeenCalledExactlyOnceWith("priority", expect.any(AbortSignal));
  });

  it.each([
    ["the window is answered", () => usePriorityAttentionStore.setState({ cue: "none" })],
    ["the sound is switched off", () => usePreferencesStore.setState({ priorityAlertSound: false })],
  ])("aborts a pending cue when %s", (_name, change) => {
    renderHook(() => usePriorityChime());
    act(() => usePriorityAttentionStore.setState({ cue: "announced", chimeCount: 5 }));
    const signal = playCue.mock.calls[0][1] as AbortSignal;
    expect(signal.aborted).toBe(false);

    act(() => change());
    expect(signal.aborted).toBe(true);
  });

  it("aborts a pending cue on unmount", () => {
    const { unmount } = renderHook(() => usePriorityChime());
    act(() => usePriorityAttentionStore.setState({ cue: "announced", chimeCount: 5 }));
    const signal = playCue.mock.calls[0][1] as AbortSignal;

    unmount();
    expect(signal.aborted).toBe(true);
  });
});
