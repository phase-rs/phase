import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { resolveGridRows, resolveSplitGridRows } from "../useResolvedGridRows.ts";
import {
  defaultFlexLayout,
  usePreferencesStore,
  type FlexLayoutConfig,
} from "../../stores/preferencesStore.ts";

// The exact literals the board grid used before Flex Layout existed. Any drift
// here is a visible layout regression for every user who never opens edit mode.
const LEGACY_DESKTOP_ROWS = "minmax(0,min(12%,100px)) 1fr minmax(0,min(18%,150px))";
const LEGACY_COMPACT_ROWS = "minmax(0,12%) 1fr minmax(0,18%)";

describe("resolveGridRows", () => {
  it("reproduces the prior hardcoded desktop layout byte-for-byte", () => {
    expect(resolveGridRows(defaultFlexLayout().gridBands, false)).toBe(LEGACY_DESKTOP_ROWS);
  });

  it("reproduces the prior hardcoded compact layout byte-for-byte", () => {
    expect(resolveGridRows(defaultFlexLayout().gridBands, true)).toBe(LEGACY_COMPACT_ROWS);
  });

  it("keeps the middle row 1fr and only varies the top/bottom bands", () => {
    const rows = resolveGridRows(
      { top: { pct: 8, pxCap: 60 }, bottom: { pct: 30, pxCap: 240 } },
      false,
    );
    expect(rows).toBe("minmax(0,min(8%,60px)) 1fr minmax(0,min(30%,240px))");
  });
});

describe("resolveSplitGridRows", () => {
  it("collapses only the top opponent row and keeps the bottom band", () => {
    expect(resolveSplitGridRows(defaultFlexLayout().gridBands, false)).toBe(
      "0px 1fr minmax(0,min(18%,150px))",
    );
  });
});

describe("preferencesStore flex layout actions", () => {
  beforeEach(() => {
    act(() => usePreferencesStore.getState().resetFlexLayout());
  });

  it("defaults to the zero-offset 'default' preset", () => {
    expect(usePreferencesStore.getState().flexLayout).toEqual(defaultFlexLayout());
  });

  it("flips activePreset to 'custom' on a manual band resize", () => {
    act(() => usePreferencesStore.getState().setFlexBand("top", { pct: 9, pxCap: 80 }));
    const { flexLayout } = usePreferencesStore.getState();
    expect(flexLayout.gridBands.top).toEqual({ pct: 9, pxCap: 80 });
    expect(flexLayout.activePreset).toBe("custom");
  });

  it("flips activePreset to 'custom' on a manual widget drag", () => {
    act(() => usePreferencesStore.getState().setFlexWidgetOffset("stackPanel", { dx: 10, dy: -20 }));
    const { flexLayout } = usePreferencesStore.getState();
    expect(flexLayout.widgets.stackPanel).toEqual({ dx: 10, dy: -20 });
    expect(flexLayout.activePreset).toBe("custom");
  });

  it("keys opponent-HUD offsets per table size and leaves the other size untouched", () => {
    act(() => usePreferencesStore.getState().setFlexOpponentHudOffset("multiplayer", { dx: 5, dy: 5 }));
    const { flexLayout } = usePreferencesStore.getState();
    expect(flexLayout.opponentHudByTableSize.multiplayer).toEqual({ dx: 5, dy: 5 });
    expect(flexLayout.opponentHudByTableSize.oneVsOne).toBeUndefined();
    expect(flexLayout.activePreset).toBe("custom");
  });

  it("sets the lands↔support ratio and flips to 'custom'", () => {
    act(() => usePreferencesStore.getState().setFlexLandSupportRatio(0.65));
    const { flexLayout } = usePreferencesStore.getState();
    expect(flexLayout.landSupportRatio).toBeCloseTo(0.65, 5);
    expect(flexLayout.activePreset).toBe("custom");
  });

  it("clamps the lands↔support ratio so neither column starves", () => {
    act(() => usePreferencesStore.getState().setFlexLandSupportRatio(0.95));
    expect(usePreferencesStore.getState().flexLayout.landSupportRatio).toBe(0.8);
    act(() => usePreferencesStore.getState().setFlexLandSupportRatio(0.05));
    expect(usePreferencesStore.getState().flexLayout.landSupportRatio).toBe(0.2);
  });

  it("sets a per-zone scale, clamps it to the readable range, and flips to 'custom'", () => {
    act(() => usePreferencesStore.getState().setFlexScale("stack", 1.4));
    let { flexLayout } = usePreferencesStore.getState();
    expect(flexLayout.scales?.stack).toBeCloseTo(1.4, 5);
    expect(flexLayout.activePreset).toBe("custom");
    // Over- and under-shoots clamp to [0.5, 2].
    act(() => usePreferencesStore.getState().setFlexScale("summaryTile", 5));
    act(() => usePreferencesStore.getState().setFlexScale("stack", 0.1));
    flexLayout = usePreferencesStore.getState().flexLayout;
    expect(flexLayout.scales?.summaryTile).toBe(2);
    expect(flexLayout.scales?.stack).toBe(0.5);
  });

  it("sets the middle-row cell order and flips to 'custom'", () => {
    act(() => usePreferencesStore.getState().setFlexMiddleRowOrder(["command", "lands", "support"]));
    const { flexLayout } = usePreferencesStore.getState();
    expect(flexLayout.middleRowOrder).toEqual(["command", "lands", "support"]);
    expect(flexLayout.activePreset).toBe("custom");
  });

  it("sets a cell's alignment and flips to 'custom'", () => {
    act(() => usePreferencesStore.getState().setFlexCellAlign("lands", "center"));
    const { flexLayout } = usePreferencesStore.getState();
    expect(flexLayout.cellAlign).toEqual({ lands: "center" });
    expect(flexLayout.activePreset).toBe("custom");
  });

  it("reset clears the ratio, order, and scales back to defaults", () => {
    act(() => {
      usePreferencesStore.getState().setFlexLandSupportRatio(0.7);
      usePreferencesStore.getState().setFlexScale("stack", 1.5);
      usePreferencesStore.getState().setFlexMiddleRowOrder(["command", "support", "lands"]);
      usePreferencesStore.getState().setFlexCellAlign("support", "start");
    });
    act(() => usePreferencesStore.getState().resetFlexLayout());
    const { flexLayout } = usePreferencesStore.getState();
    expect(flexLayout.landSupportRatio).toBe(0.5);
    expect(flexLayout.scales).toEqual({});
    expect(flexLayout.cellAlign).toEqual({});
    expect(flexLayout.middleRowOrder).toEqual(["lands", "support", "command"]);
    expect(flexLayout.activePreset).toBe("default");
  });

  it("applies a preset wholesale, including the opponent HUD, and sets its id", () => {
    // Seed a manual opponent-HUD offset, then apply a preset that doesn't carry one.
    act(() => usePreferencesStore.getState().setFlexOpponentHudOffset("oneVsOne", { dx: 40, dy: 0 }));
    const preset: FlexLayoutConfig = {
      gridBands: { top: { pct: 10, pxCap: 80 }, bottom: { pct: 14, pxCap: 120 } },
      widgets: {},
      opponentHudByTableSize: {},
      activePreset: "layout2",
    };
    act(() => usePreferencesStore.getState().applyFlexPreset(preset));
    const { flexLayout } = usePreferencesStore.getState();
    expect(flexLayout.activePreset).toBe("layout2");
    // Preset is authoritative: it wiped the manual opponent-HUD offset.
    expect(flexLayout.opponentHudByTableSize).toEqual({});
    expect(flexLayout.gridBands.top).toEqual({ pct: 10, pxCap: 80 });
  });

  it("deep-clones an applied preset so the constant can't be mutated through the store", () => {
    const preset: FlexLayoutConfig = {
      gridBands: { top: { pct: 10, pxCap: 80 }, bottom: { pct: 14, pxCap: 120 } },
      widgets: {},
      opponentHudByTableSize: {},
      activePreset: "layout2",
    };
    act(() => usePreferencesStore.getState().applyFlexPreset(preset));
    act(() => usePreferencesStore.getState().setFlexBand("top", { pct: 99, pxCap: 99 }));
    // The preset constant is unchanged despite the post-apply store mutation.
    expect(preset.gridBands.top).toEqual({ pct: 10, pxCap: 80 });
  });

  it("reset returns to the default preset and clears every offset", () => {
    act(() => {
      usePreferencesStore.getState().setFlexWidgetOffset("logPanel", { dx: 1, dy: 1 });
      usePreferencesStore.getState().setFlexBand("bottom", { pct: 25, pxCap: 200 });
    });
    act(() => usePreferencesStore.getState().resetFlexLayout());
    expect(usePreferencesStore.getState().flexLayout).toEqual(defaultFlexLayout());
  });
});

describe("preferencesStore v15→v16 migration", () => {
  afterEach(() => {
    localStorage.clear();
  });

  it("seeds the default flexLayout for a legacy store that lacks it", async () => {
    // A v15 blob with no flexLayout key, in the shape zustand/persist writes.
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({ state: { cardSize: "large", stackDockSide: "left" }, version: 15 }),
    );

    await act(async () => {
      await usePreferencesStore.persist.rehydrate();
    });

    const state = usePreferencesStore.getState();
    // The pre-existing field survived migration...
    expect(state.cardSize).toBe("large");
    // ...and the absent flexLayout was seeded to the default, resolving to the
    // identical grid as before the feature existed.
    expect(state.flexLayout).toEqual(defaultFlexLayout());
    expect(resolveGridRows(state.flexLayout.gridBands, false)).toBe(LEGACY_DESKTOP_ROWS);
  });
});
