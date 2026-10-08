import { cleanup, fireEvent, render, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { usePreferencesStore } from "../../stores/preferencesStore";
import { useUiStore } from "../../stores/uiStore";
import { useShiftHeld } from "../useShiftHeld";

function ShiftRegistration({ enabled }: { enabled: boolean }) {
  useShiftHeld(enabled);
  return null;
}

describe("useShiftHeld", () => {
  beforeEach(() => {
    usePreferencesStore.setState({ cardPreviewMode: "follow" });
    useUiStore.setState({ shiftHeld: false });
  });

  afterEach(() => {
    cleanup();
    useUiStore.setState({ shiftHeld: false });
  });

  it("tracks explicit enablement independently of the global preview mode", () => {
    const { rerender, unmount } = renderHook(
      ({ enabled }) => useShiftHeld(enabled),
      { initialProps: { enabled: true } },
    );

    fireEvent.keyDown(window, { key: "Control" });
    expect(useUiStore.getState().shiftHeld).toBe(false);
    fireEvent.keyDown(window, { key: "Shift" });
    expect(useUiStore.getState().shiftHeld).toBe(true);
    fireEvent.keyUp(window, { key: "Shift" });
    expect(useUiStore.getState().shiftHeld).toBe(false);
    fireEvent.keyDown(window, { key: "Shift" });
    fireEvent(window, new Event("blur"));
    expect(useUiStore.getState().shiftHeld).toBe(false);

    fireEvent.keyDown(window, { key: "Shift" });
    rerender({ enabled: false });
    expect(useUiStore.getState().shiftHeld).toBe(false);
    fireEvent.keyDown(window, { key: "Shift" });
    expect(useUiStore.getState().shiftHeld).toBe(false);
    unmount();
  });

  it("keeps shared held state until the final enabled registration is removed", () => {
    const { rerender } = render(
      <>
        <ShiftRegistration enabled />
        <ShiftRegistration enabled />
      </>,
    );

    fireEvent.keyDown(window, { key: "Shift" });
    expect(useUiStore.getState().shiftHeld).toBe(true);

    rerender(<ShiftRegistration enabled />);
    expect(useUiStore.getState().shiftHeld).toBe(true);
    fireEvent.keyUp(window, { key: "Shift" });
    expect(useUiStore.getState().shiftHeld).toBe(false);

    fireEvent.keyDown(window, { key: "Shift" });
    rerender(<ShiftRegistration enabled={false} />);
    expect(useUiStore.getState().shiftHeld).toBe(false);
  });

  it("preserves global-mode behavior when explicit enablement is omitted", () => {
    usePreferencesStore.setState({ cardPreviewMode: "shift" });
    const { unmount } = renderHook(() => useShiftHeld());

    fireEvent.keyDown(window, { key: "Shift" });
    expect(useUiStore.getState().shiftHeld).toBe(true);
    unmount();
    expect(useUiStore.getState().shiftHeld).toBe(false);
  });

  it("lets explicit false disable a hostile global Shift mode", () => {
    usePreferencesStore.setState({ cardPreviewMode: "shift" });
    useUiStore.setState({ shiftHeld: true });
    renderHook(() => useShiftHeld(false));

    expect(useUiStore.getState().shiftHeld).toBe(false);
    fireEvent.keyDown(window, { key: "Shift" });
    expect(useUiStore.getState().shiftHeld).toBe(false);
  });
});