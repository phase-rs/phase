import { act } from "react";
import { beforeEach, describe, expect, it } from "vitest";

import { useUiStore } from "../uiStore";

describe("uiStore", () => {
  beforeEach(() => {
    act(() => {
      useUiStore.setState({
        selectedObjectId: null,
        hoveredObjectId: null,
        inspectedObjectId: null,
        inspectedFaceIndex: 0,
        altHeld: false,
        selectedCardIds: [],
        fullControl: false,
        autoPass: false,
      });
    });
  });

  it("selectObject sets selectedObjectId", () => {
    act(() => useUiStore.getState().selectObject(42));
    expect(useUiStore.getState().selectedObjectId).toBe(42);
  });

  it("hoverObject sets hoveredObjectId", () => {
    act(() => useUiStore.getState().hoverObject(7));
    expect(useUiStore.getState().hoveredObjectId).toBe(7);
  });

  it("inspectObject sets inspectedObjectId", () => {
    act(() => useUiStore.getState().inspectObject(99));
    expect(useUiStore.getState().inspectedObjectId).toBe(99);
  });

  it("inspecting a different object resets a pinned altHeld", () => {
    act(() => {
      useUiStore.getState().inspectObject(1);
      useUiStore.getState().setAltHeld(true);
    });
    expect(useUiStore.getState().altHeld).toBe(true);

    // Switching to a new card dismisses the old preview — Alt must not leak.
    act(() => useUiStore.getState().inspectObject(2));
    expect(useUiStore.getState().inspectedObjectId).toBe(2);
    expect(useUiStore.getState().altHeld).toBe(false);
  });

  it("re-inspecting the same object preserves a pinned altHeld", () => {
    act(() => {
      useUiStore.getState().inspectObject(5);
      useUiStore.getState().setAltHeld(true);
    });

    // Re-hover / face flip on the same card keeps the reader pinned.
    act(() => useUiStore.getState().inspectObject(5, 1));
    expect(useUiStore.getState().inspectedObjectId).toBe(5);
    expect(useUiStore.getState().inspectedFaceIndex).toBe(1);
    expect(useUiStore.getState().altHeld).toBe(true);
  });

  it("addSelectedCard appends to selectedCardIds", () => {
    act(() => {
      useUiStore.getState().addSelectedCard(5);
      useUiStore.getState().addSelectedCard(10);
    });
    expect(useUiStore.getState().selectedCardIds).toEqual([5, 10]);
  });

  it("cycleSelectedCard deselects an already-selected card", () => {
    act(() => {
      useUiStore.getState().cycleSelectedCard(5, 2);
      useUiStore.getState().cycleSelectedCard(10, 2);
      useUiStore.getState().cycleSelectedCard(5, 2);
    });
    expect(useUiStore.getState().selectedCardIds).toEqual([10]);
  });

  it("cycleSelectedCard adds while under the cap", () => {
    act(() => {
      useUiStore.getState().cycleSelectedCard(5, 2);
      useUiStore.getState().cycleSelectedCard(10, 2);
    });
    expect(useUiStore.getState().selectedCardIds).toEqual([5, 10]);
  });

  it("cycleSelectedCard swaps the single selection at max === 1", () => {
    act(() => {
      useUiStore.getState().cycleSelectedCard(5, 1);
      useUiStore.getState().cycleSelectedCard(10, 1);
    });
    expect(useUiStore.getState().selectedCardIds).toEqual([10]);
  });

  it("cycleSelectedCard evicts the oldest selection at the cap", () => {
    act(() => {
      useUiStore.getState().cycleSelectedCard(5, 2);
      useUiStore.getState().cycleSelectedCard(10, 2);
      useUiStore.getState().cycleSelectedCard(15, 2);
    });
    expect(useUiStore.getState().selectedCardIds).toEqual([10, 15]);
  });

  it("clearSelectedCards resets selectedCardIds", () => {
    act(() => {
      useUiStore.getState().addSelectedCard(1);
      useUiStore.getState().clearSelectedCards();
    });

    expect(useUiStore.getState().selectedCardIds).toEqual([]);
  });

  it("toggleFullControl flips fullControl boolean", () => {
    expect(useUiStore.getState().fullControl).toBe(false);
    act(() => useUiStore.getState().toggleFullControl());
    expect(useUiStore.getState().fullControl).toBe(true);
    act(() => useUiStore.getState().toggleFullControl());
    expect(useUiStore.getState().fullControl).toBe(false);
  });

  it("toggleAutoPass flips autoPass boolean", () => {
    expect(useUiStore.getState().autoPass).toBe(false);
    act(() => useUiStore.getState().toggleAutoPass());
    expect(useUiStore.getState().autoPass).toBe(true);
  });

  it("toggleDebugClickModeButtonVisible flips the pinned click-mode control", () => {
    expect(useUiStore.getState().debugClickModeButtonVisible).toBe(false);
    act(() => useUiStore.getState().toggleDebugClickModeButtonVisible());
    expect(useUiStore.getState().debugClickModeButtonVisible).toBe(true);
    act(() => useUiStore.getState().toggleDebugClickModeButtonVisible());
    expect(useUiStore.getState().debugClickModeButtonVisible).toBe(false);
  });
});
