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

  it("addSelectedCard appends to selectedCardIds", () => {
    act(() => {
      useUiStore.getState().addSelectedCard(5);
      useUiStore.getState().addSelectedCard(10);
    });
    expect(useUiStore.getState().selectedCardIds).toEqual([5, 10]);
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
});
