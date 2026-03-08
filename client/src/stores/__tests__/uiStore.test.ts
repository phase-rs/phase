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
        targetingMode: false,
        validTargetIds: [],
        sourceObjectId: null,
        selectedTargets: [],
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

  it("startTargeting enters targeting mode", () => {
    act(() => useUiStore.getState().startTargeting([1, 2, 3], 10));

    const state = useUiStore.getState();
    expect(state.targetingMode).toBe(true);
    expect(state.validTargetIds).toEqual([1, 2, 3]);
    expect(state.sourceObjectId).toBe(10);
    expect(state.selectedTargets).toEqual([]);
  });

  it("addTarget appends to selectedTargets", () => {
    act(() => {
      useUiStore.getState().addTarget(5);
      useUiStore.getState().addTarget(10);
    });
    expect(useUiStore.getState().selectedTargets).toEqual([5, 10]);
  });

  it("clearTargets resets targeting state", () => {
    act(() => {
      useUiStore.getState().startTargeting([1, 2], 5);
      useUiStore.getState().addTarget(1);
      useUiStore.getState().clearTargets();
    });

    const state = useUiStore.getState();
    expect(state.targetingMode).toBe(false);
    expect(state.validTargetIds).toEqual([]);
    expect(state.sourceObjectId).toBeNull();
    expect(state.selectedTargets).toEqual([]);
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
