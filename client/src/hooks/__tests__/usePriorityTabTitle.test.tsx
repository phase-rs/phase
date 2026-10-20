// @vitest-environment jsdom

import { act, cleanup, renderHook } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore } from "../../stores/preferencesStore";
import { usePriorityAttentionStore } from "../../stores/priorityAttentionStore";
import { usePriorityTabTitle } from "../usePriorityTabTitle";

const ORIGINAL_TITLE = "phase.rs — Match 7";
const ALERT_TITLE = `● Your priority — ${ORIGINAL_TITLE}`;
let hidden = false;

function setHidden(next: boolean): void {
  hidden = next;
  document.dispatchEvent(new Event("visibilitychange"));
}

beforeEach(() => {
  hidden = false;
  vi.spyOn(document, "hidden", "get").mockImplementation(() => hidden);
  vi.spyOn(document, "visibilityState", "get").mockImplementation(() =>
    hidden ? "hidden" : "visible",
  );
  document.title = ORIGINAL_TITLE;
  usePreferencesStore.setState({ priorityAlertTitle: true });
  usePriorityAttentionStore.setState({ cue: "announced", chimeCount: 1 });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  document.title = "";
  usePreferencesStore.setState({ priorityAlertTitle: true });
  usePriorityAttentionStore.setState({ cue: "none", chimeCount: 0 });
});

describe("usePriorityTabTitle", () => {
  it("applies the translated cue to a hidden tab", () => {
    hidden = true;
    renderHook(() => usePriorityTabTitle());
    expect(document.title).toBe(ALERT_TITLE);
  });

  it("leaves a visible tab unchanged", () => {
    renderHook(() => usePriorityTabTitle());
    expect(document.title).toBe(ORIGINAL_TITLE);
  });

  it("leaves a hidden tab unchanged when no announcement is pending", () => {
    hidden = true;
    usePriorityAttentionStore.setState({ cue: "none" });
    renderHook(() => usePriorityTabTitle());
    expect(document.title).toBe(ORIGINAL_TITLE);

    act(() => usePriorityAttentionStore.setState({ cue: "announced" }));
    expect(document.title).toBe(ALERT_TITLE);
  });

  it("honors the disabled preference and applies when it is enabled", () => {
    hidden = true;
    usePreferencesStore.setState({ priorityAlertTitle: false });
    renderHook(() => usePriorityTabTitle());
    expect(document.title).toBe(ORIGINAL_TITLE);

    act(() => usePreferencesStore.getState().setPriorityAlertTitle(true));
    expect(document.title).toBe(ALERT_TITLE);
  });

  it("captures the title when applying, not when mounting", () => {
    renderHook(() => usePriorityTabTitle());
    document.title = "A different match";
    act(() => setHidden(true));
    expect(document.title).toBe("● Your priority — A different match");

    act(() => setHidden(false));
    expect(document.title).toBe("A different match");
  });

  it("restores on visibility changes and reapplies without stacking", () => {
    hidden = true;
    renderHook(() => usePriorityTabTitle());
    expect(document.title).toBe(ALERT_TITLE);

    act(() => setHidden(false));
    expect(document.title).toBe(ORIGINAL_TITLE);
    act(() => setHidden(true));
    act(() => setHidden(true));
    expect(document.title).toBe(ALERT_TITLE);
  });

  it("restores on focus even before visibilityState catches up", () => {
    hidden = true;
    renderHook(() => usePriorityTabTitle());
    expect(document.title).toBe(ALERT_TITLE);

    act(() => window.dispatchEvent(new Event("focus")));
    expect(document.title).toBe(ORIGINAL_TITLE);
  });

  it("restores when the announcement ends", () => {
    hidden = true;
    renderHook(() => usePriorityTabTitle());
    expect(document.title).toBe(ALERT_TITLE);

    act(() => usePriorityAttentionStore.setState({ cue: "none" }));
    expect(document.title).toBe(ORIGINAL_TITLE);
  });

  it("restores when the preference is turned off", () => {
    hidden = true;
    renderHook(() => usePriorityTabTitle());
    expect(document.title).toBe(ALERT_TITLE);

    act(() => usePreferencesStore.getState().setPriorityAlertTitle(false));
    expect(document.title).toBe(ORIGINAL_TITLE);
  });

  it("restores on unmount and removes visibility listeners", () => {
    hidden = true;
    const { unmount } = renderHook(() => usePriorityTabTitle());
    expect(document.title).toBe(ALERT_TITLE);

    unmount();
    expect(document.title).toBe(ORIGINAL_TITLE);
    act(() => setHidden(false));
    act(() => setHidden(true));
    expect(document.title).toBe(ORIGINAL_TITLE);
  });

  it.each(["visible", "focus", "alerting", "preference", "unmount"] as const)(
    "preserves a foreign title when restoring for %s",
    (reason) => {
      hidden = true;
      const { unmount } = renderHook(() => usePriorityTabTitle());
      expect(document.title).toBe(ALERT_TITLE);
      document.title = "Owned by another feature";

      act(() => {
        switch (reason) {
          case "visible":
            setHidden(false);
            break;
          case "focus":
            window.dispatchEvent(new Event("focus"));
            break;
          case "alerting":
            usePriorityAttentionStore.setState({ cue: "none" });
            break;
          case "preference":
            usePreferencesStore.getState().setPriorityAlertTitle(false);
            break;
          case "unmount":
            unmount();
            break;
        }
      });
      expect(document.title).toBe("Owned by another feature");
    },
  );

  it("survives StrictMode's setup-cleanup-setup cycle", () => {
    hidden = true;
    const { unmount } = renderHook(() => usePriorityTabTitle(), { wrapper: StrictMode });
    expect(document.title).toBe(ALERT_TITLE);
    unmount();
    expect(document.title).toBe(ORIGINAL_TITLE);
  });

  it("keeps its ownership when the cue grows from announced to reminded", () => {
    hidden = true;
    renderHook(() => usePriorityTabTitle());
    expect(document.title).toBe(ALERT_TITLE);

    // Another feature takes the title over while the window is still pending;
    // the stronger reminder must not reclaim it.
    document.title = "Owned by another feature";
    act(() => usePriorityAttentionStore.setState({ cue: "reminded" }));
    expect(document.title).toBe("Owned by another feature");
    act(() => usePriorityAttentionStore.setState({ cue: "announced" }));
    expect(document.title).toBe("Owned by another feature");
  });
});
