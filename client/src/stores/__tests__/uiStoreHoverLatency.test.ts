import { describe, expect, it, beforeEach, afterEach, vi } from "vitest";

import { useUiStore } from "../uiStore";
import { usePreferencesStore } from "../preferencesStore";

/**
 * Covers the configurable card-hover latency wired into `inspectObject`. The
 * delay is the whole feature, so these exercise the branches a regression would
 * silently break: delayed first show, cancel-on-hover-out, instant retarget
 * while a preview is already open, the "shift" bind-key exclusion, and the
 * "immediate" timing bypass used by long-press.
 */
describe("uiStore inspectObject hover latency", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // jsdom has no matchMedia; the latency only applies on hover-capable
    // devices, so stub "(hover: hover)" → true for these tests.
    window.matchMedia = ((query: string) => ({
      matches: true,
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    })) as unknown as typeof window.matchMedia;
    useUiStore.getState().dismissPreview();
    useUiStore.setState({ inspectedObjectId: null });
    usePreferencesStore.setState({ cardPreviewMode: "follow", cardPreviewHoverDelayMs: 0 });
  });

  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it("defers the first preview by cardPreviewHoverDelayMs", () => {
    usePreferencesStore.setState({ cardPreviewHoverDelayMs: 300 });
    useUiStore.getState().inspectObject(5);

    expect(useUiStore.getState().inspectedObjectId).toBeNull();
    vi.advanceTimersByTime(299);
    expect(useUiStore.getState().inspectedObjectId).toBeNull();
    vi.advanceTimersByTime(1);
    expect(useUiStore.getState().inspectedObjectId).toBe(5);
  });

  it("shows instantly when the delay is 0 (default)", () => {
    useUiStore.getState().inspectObject(7);
    expect(useUiStore.getState().inspectedObjectId).toBe(7);
  });

  it("cancels a pending show when the cursor leaves before the delay elapses", () => {
    usePreferencesStore.setState({ cardPreviewHoverDelayMs: 300 });
    useUiStore.getState().inspectObject(5);
    vi.advanceTimersByTime(100);
    useUiStore.getState().inspectObject(null);
    vi.advanceTimersByTime(500);
    expect(useUiStore.getState().inspectedObjectId).toBeNull();
  });

  it("switches instantly while a preview is already open", () => {
    usePreferencesStore.setState({ cardPreviewHoverDelayMs: 300 });
    useUiStore.getState().inspectObject(5);
    vi.advanceTimersByTime(300);
    expect(useUiStore.getState().inspectedObjectId).toBe(5);

    // Retarget without leaving: a preview is open, so the swap is immediate.
    useUiStore.getState().inspectObject(6);
    expect(useUiStore.getState().inspectedObjectId).toBe(6);
  });

  it("ignores the latency in shift bind-key mode", () => {
    usePreferencesStore.setState({ cardPreviewMode: "shift", cardPreviewHoverDelayMs: 300 });
    useUiStore.getState().inspectObject(5);
    expect(useUiStore.getState().inspectedObjectId).toBe(5);
  });

  it("bypasses the latency for immediate (long-press) inspects", () => {
    usePreferencesStore.setState({ cardPreviewHoverDelayMs: 300 });
    useUiStore.getState().inspectObject(5, undefined, "immediate");
    expect(useUiStore.getState().inspectedObjectId).toBe(5);
  });
});
