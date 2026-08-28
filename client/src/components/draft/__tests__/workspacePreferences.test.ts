import { beforeEach, describe, expect, it, vi } from "vitest";
import { DRAFT_WORKSPACE_PREFERENCES_KEY } from "../../../constants/storage";
import {
  createDefaultDraftWorkspacePreferences,
  DRAFT_WORKSPACE_PACK_SCALE_DEFAULT,
  DRAFT_WORKSPACE_PACK_SCALE_MAX,
  DRAFT_WORKSPACE_PACK_SCALE_MIN,
  DRAFT_WORKSPACE_PACK_SCALE_STEP,
  getResponsiveDraftLayout,
  loadDraftWorkspacePreferences,
  repairDraftWorkspacePreferences,
  resolveDraftWorkspaceSideboardCollapsed,
  resolveDraftWorkspaceVisualColumnCap,
  resolveDraftWorkspaceView,
  saveDraftWorkspacePreferences,
} from "../workspace/workspacePreferences";

beforeEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
});

describe("workspace preferences", () => {
  it("repairs_malformed_preferences_field_by_field_and_clamps_dimensions", () => {
    const repaired = repairDraftWorkspacePreferences({
      schemaVersion: 3,
      explicitView: "board",
      cardPreviewMode: "invalid",
      packScale: 3,
      pinned: "yes",
      pinnedHeightRatio: 2,
      sideboardCollapsed: false,
      builderPhoneSideboardCollapsed: false,
      phoneDeckVisualColumnCaps: { portrait: 0, landscape: 7 },
      tabletDeckVisualColumnCaps: { portrait: 0, landscape: 16 },
      deck: {
        sort: "type",
        columnCount: 1,
        rows: "two",
        showHeaders: false,
      },
      sideboard: {
        sort: "invalid",
        columnCount: 21,
        rows: 2,
        showHeaders: true,
      },
    });

    expect(repaired).toEqual({
      schemaVersion: 3,
      explicitView: "board",
      cardPreviewMode: "none",
      packScale: 2.9,
      sideboardCollapsed: false,
      builderPhoneSideboardCollapsed: false,
      phoneDeckVisualColumnCaps: { portrait: 1, landscape: 5 },
      tabletDeckVisualColumnCaps: { portrait: 1, landscape: 15 },
      deck: { sort: "type", columnCount: 2, rows: "two", showHeaders: false },
      sideboard: { sort: "cmc", columnCount: 20, rows: "one", showHeaders: true },
    });
  });

  it("resolves_live_width_breakpoints", () => {
    for (const layout of ["phone-portrait", "phone-landscape", "tablet-portrait", "tablet-landscape"] as const) {
      expect(resolveDraftWorkspaceView(null, 1024, layout)).toBe("compact");
    }
    expect(resolveDraftWorkspaceView(null, 1440, "desktop")).toBe("board");
    expect(resolveDraftWorkspaceView("compact", 1440)).toBe("compact");
    expect(resolveDraftWorkspaceView("board", 390)).toBe("board");
  });

  it("classifies the responsive drafting layouts by viewport dimensions", () => {
    expect(getResponsiveDraftLayout(390, 844)).toBe("phone-portrait");
    expect(getResponsiveDraftLayout(844, 390)).toBe("phone-landscape");
    expect(getResponsiveDraftLayout(924, 412)).toBe("phone-landscape");
    expect(getResponsiveDraftLayout(768, 1024)).toBe("tablet-portrait");
    expect(getResponsiveDraftLayout(1024, 768)).toBe("tablet-landscape");
    expect(getResponsiveDraftLayout(1440, 900)).toBe("desktop");
  });

  it("caps_visual_column_groups_by_context_and_orientation", () => {
    const phoneCaps = { portrait: 3, landscape: 5 };
    const tabletCaps = { portrait: 12, landscape: 15 };
    expect(resolveDraftWorkspaceVisualColumnCap("phone-portrait", "draft", phoneCaps, tabletCaps)).toBe(3);
    expect(resolveDraftWorkspaceVisualColumnCap("phone-landscape", "draft", phoneCaps, tabletCaps)).toBe(5);
    expect(resolveDraftWorkspaceVisualColumnCap("phone-portrait", "builder", phoneCaps, tabletCaps)).toBe(3);
    expect(resolveDraftWorkspaceVisualColumnCap("phone-landscape", "builder", phoneCaps, tabletCaps)).toBe(5);
    expect(resolveDraftWorkspaceVisualColumnCap("tablet-portrait", "draft", phoneCaps, tabletCaps)).toBe(3);
    expect(resolveDraftWorkspaceVisualColumnCap("tablet-landscape", "draft", phoneCaps, tabletCaps)).toBe(5);
    expect(resolveDraftWorkspaceVisualColumnCap("tablet-portrait", "builder", phoneCaps, tabletCaps)).toBe(12);
    expect(resolveDraftWorkspaceVisualColumnCap("tablet-landscape", "builder", phoneCaps, tabletCaps)).toBe(15);
    expect(resolveDraftWorkspaceVisualColumnCap("desktop", "draft", phoneCaps, tabletCaps)).toBeUndefined();
  });

  it("honors_draft_phone_tablet_and_desktop_sideboard_overrides", () => {
    for (const viewportWidth of [390, 1024, 1199, 1200, 1440]) {
      expect(resolveDraftWorkspaceSideboardCollapsed(null, viewportWidth)).toBe(true);
    }
    expect(resolveDraftWorkspaceSideboardCollapsed(false, 430, "phone-portrait")).toBe(false);
    expect(resolveDraftWorkspaceSideboardCollapsed(false, 844, "phone-landscape")).toBe(false);
    expect(resolveDraftWorkspaceSideboardCollapsed(null, 430, "phone-portrait")).toBe(true);
    expect(resolveDraftWorkspaceSideboardCollapsed(null, 844, "phone-landscape")).toBe(false);
    expect(resolveDraftWorkspaceSideboardCollapsed(false, 768, "tablet-portrait")).toBe(false);
    expect(resolveDraftWorkspaceSideboardCollapsed(false, 1024, "tablet-landscape")).toBe(false);
    expect(resolveDraftWorkspaceSideboardCollapsed(null, 768, "tablet-portrait")).toBe(true);
    expect(resolveDraftWorkspaceSideboardCollapsed(null, 1024, "tablet-landscape")).toBe(true);
    expect(resolveDraftWorkspaceSideboardCollapsed(false, 1440, "desktop")).toBe(false);
    expect(resolveDraftWorkspaceSideboardCollapsed(false, 390, "phone-portrait", "builder", false)).toBe(false);
    expect(resolveDraftWorkspaceSideboardCollapsed(false, 844, "phone-landscape", "builder", false)).toBe(false);
  });

  it("returns fresh exact defaults and repairs nested boards independently", () => {
    const first = createDefaultDraftWorkspacePreferences();
    const second = createDefaultDraftWorkspacePreferences();
    expect(first).toEqual({
      schemaVersion: 3,
      explicitView: null,
      cardPreviewMode: "none",
      packScale: DRAFT_WORKSPACE_PACK_SCALE_DEFAULT,
      sideboardCollapsed: null,
      builderPhoneSideboardCollapsed: true,
      phoneDeckVisualColumnCaps: { portrait: 3, landscape: 5 },
      tabletDeckVisualColumnCaps: { portrait: 3, landscape: 5 },
      deck: { sort: "cmc", columnCount: 7, rows: "one", showHeaders: true },
      sideboard: { sort: "cmc", columnCount: 6, rows: "one", showHeaders: true },
    });
    expect(first.deck).not.toBe(second.deck);
    expect(first.sideboard).not.toBe(second.sideboard);

    expect(repairDraftWorkspacePreferences({
      ...first,
      deck: null,
      sideboard: { ...first.sideboard, sort: "rarity", columnCount: 9 },
    })).toMatchObject({
      deck: first.deck,
      sideboard: { sort: "rarity", columnCount: 9 },
    });
  });

  it("defaults invalid roots, versions, fractions, and non-finite values", () => {
    const defaults = createDefaultDraftWorkspacePreferences();
    for (const value of [null, [], "invalid", {}, { schemaVersion: 4 }]) {
      expect(repairDraftWorkspacePreferences(value)).toEqual(defaults);
    }
    expect(repairDraftWorkspacePreferences({
      ...defaults,
      packScale: Number.NaN,
      deck: { ...defaults.deck, columnCount: 3.5 },
      sideboard: { ...defaults.sideboard, columnCount: Number.POSITIVE_INFINITY },
    })).toMatchObject({
      packScale: 1.65,
      deck: { columnCount: 7 },
      sideboard: { columnCount: 6 },
    });
  });

  it("migrates_v1_and_v2_preferences_and_repairs_v3_visual_caps_independently", () => {
    expect(repairDraftWorkspacePreferences({
      schemaVersion: 1,
      explicitView: "board",
      cardPreviewMode: "follow",
      packScale: 1.8,
      sideboardCollapsed: false,
      deck: { sort: "type", columnCount: 4, rows: "two", showHeaders: false },
      sideboard: { sort: "color", columnCount: 8, rows: "one", showHeaders: true },
    })).toEqual({
      schemaVersion: 3,
      explicitView: "board",
      cardPreviewMode: "follow",
      packScale: 1.8,
      sideboardCollapsed: false,
      builderPhoneSideboardCollapsed: true,
      phoneDeckVisualColumnCaps: { portrait: 3, landscape: 5 },
      tabletDeckVisualColumnCaps: { portrait: 3, landscape: 5 },
      deck: { sort: "type", columnCount: 4, rows: "two", showHeaders: false },
      sideboard: { sort: "color", columnCount: 8, rows: "one", showHeaders: true },
    });

    expect(repairDraftWorkspacePreferences({
      schemaVersion: 2,
      explicitView: "board",
      cardPreviewMode: "none",
      packScale: 1.65,
      sideboardCollapsed: null,
      builderPhoneSideboardCollapsed: true,
      phoneDeckVisualColumnCaps: { portrait: 2, landscape: 5 },
      deck: { sort: "cmc", columnCount: 7, rows: "one", showHeaders: true },
      sideboard: { sort: "cmc", columnCount: 6, rows: "one", showHeaders: true },
    })).toMatchObject({
      schemaVersion: 3,
      phoneDeckVisualColumnCaps: { portrait: 2, landscape: 5 },
      tabletDeckVisualColumnCaps: { portrait: 2, landscape: 5 },
    });

    const defaults = createDefaultDraftWorkspacePreferences();
    expect(repairDraftWorkspacePreferences({
      ...defaults,
      builderPhoneSideboardCollapsed: "false",
      phoneDeckVisualColumnCaps: { portrait: 2.5, landscape: 2 },
      tabletDeckVisualColumnCaps: { portrait: 2.5, landscape: 16 },
    })).toMatchObject({
      builderPhoneSideboardCollapsed: true,
      phoneDeckVisualColumnCaps: { portrait: 3, landscape: 2 },
      tabletDeckVisualColumnCaps: { portrait: 3, landscape: 15 },
    });
  });

  it("repairs_pack_scale_to_inclusive_hundredths", () => {
    const defaults = createDefaultDraftWorkspacePreferences();
    expect(repairDraftWorkspacePreferences({ ...defaults, packScale: 0.39 }).packScale).toBe(0.4);
    expect(repairDraftWorkspacePreferences({ ...defaults, packScale: 2.91 }).packScale).toBe(2.9);
    expect(repairDraftWorkspacePreferences({ ...defaults, packScale: 0.734 }).packScale).toBe(0.73);
    expect(repairDraftWorkspacePreferences({ ...defaults, packScale: 0.735 }).packScale).toBe(0.74);
    expect(repairDraftWorkspacePreferences({ ...defaults, packScale: Number.POSITIVE_INFINITY }).packScale).toBe(1.65);
    expect((Math.round(DRAFT_WORKSPACE_PACK_SCALE_MAX * 100) - Math.round(DRAFT_WORKSPACE_PACK_SCALE_MIN * 100))
      / Math.round(DRAFT_WORKSPACE_PACK_SCALE_STEP * 100)).toBe(250);
    expect(DRAFT_WORKSPACE_PACK_SCALE_DEFAULT).toBe(
      (DRAFT_WORKSPACE_PACK_SCALE_MIN + DRAFT_WORKSPACE_PACK_SCALE_MAX) / 2,
    );
    expect(250 + 1).toBe(251);

    expect(saveDraftWorkspacePreferences({ ...defaults, packScale: 0.85 })).toBe("saved");
    expect(loadDraftWorkspacePreferences().packScale).toBe(0.85);
  });

  it("ignores_legacy_pin_fields_without_rewriting_storage", () => {
    const setItem = vi.spyOn(Storage.prototype, "setItem");
    localStorage.setItem(DRAFT_WORKSPACE_PREFERENCES_KEY, JSON.stringify({
      ...createDefaultDraftWorkspacePreferences(),
      pinned: true,
      pinnedHeightRatio: 0.6,
    }));
    setItem.mockClear();

    expect(loadDraftWorkspacePreferences()).toEqual(createDefaultDraftWorkspacePreferences());
    expect(setItem).not.toHaveBeenCalled();
  });

  it("defaults malformed preview modes and round-trips every valid mode", () => {
    const defaults = createDefaultDraftWorkspacePreferences();
    expect(defaults.cardPreviewMode).toBe("none");
    expect(repairDraftWorkspacePreferences({ ...defaults, cardPreviewMode: undefined }).cardPreviewMode)
      .toBe("none");
    expect(repairDraftWorkspacePreferences({ ...defaults, cardPreviewMode: "invalid" }).cardPreviewMode)
      .toBe("none");

    for (const cardPreviewMode of ["none", "follow", "side", "shift"] as const) {
      expect(saveDraftWorkspacePreferences({ ...defaults, cardPreviewMode })).toBe("saved");
      expect(loadDraftWorkspacePreferences()).toEqual({ ...defaults, cardPreviewMode });
    }
  });

  it("honors explicit responsive overrides", () => {
    expect(resolveDraftWorkspaceView("compact", 2000)).toBe("compact");
    expect(resolveDraftWorkspaceView("board", 320)).toBe("board");
    expect(resolveDraftWorkspaceSideboardCollapsed(false, 2000)).toBe(false);
    expect(resolveDraftWorkspaceSideboardCollapsed(true, 320)).toBe(true);
  });

  it("loads and saves repaired preferences without throwing on storage failures", () => {
    const defaults = createDefaultDraftWorkspacePreferences();
    localStorage.setItem(DRAFT_WORKSPACE_PREFERENCES_KEY, "not json");
    expect(loadDraftWorkspacePreferences()).toEqual(defaults);

    expect(saveDraftWorkspacePreferences({ ...defaults, explicitView: "board" })).toBe("saved");
    expect(loadDraftWorkspacePreferences().explicitView).toBe("board");

    vi.spyOn(localStorage, "getItem").mockImplementation(() => {
      throw new Error("unavailable");
    });
    expect(loadDraftWorkspacePreferences()).toEqual(defaults);
    vi.restoreAllMocks();

    vi.spyOn(localStorage, "setItem").mockImplementation(() => {
      throw new Error("quota");
    });
    expect(saveDraftWorkspacePreferences(defaults)).toBe("storage-unavailable");
  });
});
