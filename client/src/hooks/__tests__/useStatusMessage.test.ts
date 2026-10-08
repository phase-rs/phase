import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useStatusMessage } from "../useStatusMessage";
import { fetchStatus, type StatusMessage } from "../../services/status";
import { usePreferencesStore } from "../../stores/preferencesStore";

vi.mock("../../services/status", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/status")>()),
  fetchStatus: vi.fn(),
}));

const BASE: StatusMessage = {
  id: 1756482000000,
  severity: "warning",
  title: "Multiplayer maintenance",
  body: "The lobby restarts at 20:00 UTC.",
  dismissible: true,
};

// happy-dom exposes `visibilityState` as a getter, so vi.stubGlobal cannot
// replace it — redefine the property and drive it through this variable. There
// is no in-repo precedent for this: every production read is read-only.
let visibility: DocumentVisibilityState = "visible";
Object.defineProperty(document, "visibilityState", {
  configurable: true,
  get: () => visibility,
});

function setVisibility(next: DocumentVisibilityState) {
  visibility = next;
  document.dispatchEvent(new Event("visibilitychange"));
}

/** Let the mocked fetch's promise chain settle inside act(). */
async function settle() {
  await act(async () => {
    await Promise.resolve();
  });
}

async function advance(ms: number) {
  await act(async () => {
    vi.advanceTimersByTime(ms);
  });
  await settle();
}

beforeEach(() => {
  vi.useFakeTimers();
  visibility = "visible";
  localStorage.clear();
  usePreferencesStore.setState({ dismissedStatusId: undefined });
  vi.mocked(fetchStatus).mockResolvedValue(BASE);
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.clearAllMocks();
});

describe("useStatusMessage", () => {
  it("fetches on mount and re-polls once a minute while the tab is visible", async () => {
    const { result } = renderHook(() => useStatusMessage());
    await settle();

    expect(fetchStatus).toHaveBeenCalledTimes(1);
    expect(result.current).toEqual(BASE);

    await advance(60_000);
    expect(fetchStatus).toHaveBeenCalledTimes(2);

    await advance(60_000);
    expect(fetchStatus).toHaveBeenCalledTimes(3);
  });

  it("never polls while the tab is hidden", async () => {
    visibility = "hidden";
    renderHook(() => useStatusMessage());
    await settle();

    expect(fetchStatus).not.toHaveBeenCalled();

    await advance(180_000);
    expect(fetchStatus).not.toHaveBeenCalled();
  });

  it("catches up as soon as a hidden tab becomes visible again", async () => {
    visibility = "hidden";
    const { result } = renderHook(() => useStatusMessage());
    await settle();
    await advance(120_000);
    expect(fetchStatus).not.toHaveBeenCalled();

    await act(async () => {
      setVisibility("visible");
    });
    await settle();

    expect(fetchStatus).toHaveBeenCalledTimes(1);
    expect(result.current).toEqual(BASE);
  });

  it("clears both the interval and the visibilitychange listener on unmount", async () => {
    const { unmount } = renderHook(() => useStatusMessage());
    await settle();
    expect(fetchStatus).toHaveBeenCalledTimes(1);

    unmount();

    await advance(180_000);
    await act(async () => {
      setVisibility("hidden");
      setVisibility("visible");
    });
    await settle();

    // Still 1: a leaked interval or listener would have polled again.
    expect(fetchStatus).toHaveBeenCalledTimes(1);
  });

  it("drops an expired message on the next tick even when the payload is unchanged", async () => {
    // Inside the max-age=60 freshness window a poll is served from the HTTP
    // cache and returns a byte-identical payload, so expiry must be re-checked
    // against a fresh clock rather than memoized on payload identity.
    const message: StatusMessage = {
      ...BASE,
      expiresAt: new Date(Date.now() + 30_000).toISOString(),
    };
    vi.mocked(fetchStatus).mockResolvedValue(message);

    const { result } = renderHook(() => useStatusMessage());
    await settle();
    expect(result.current).toEqual(message);

    await advance(60_000);

    expect(fetchStatus).toHaveBeenCalledTimes(2);
    expect(result.current).toBeNull();
  });

  it("suppresses a message the player has already dismissed", async () => {
    usePreferencesStore.setState({ dismissedStatusId: BASE.id });
    const { result } = renderHook(() => useStatusMessage());
    await settle();

    expect(result.current).toBeNull();
  });
});
