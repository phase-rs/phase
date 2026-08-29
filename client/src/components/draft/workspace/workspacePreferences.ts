import { DRAFT_WORKSPACE_PREFERENCES_KEY } from "../../../constants/storage";
import type { CardPreviewMode } from "../../../stores/preferencesStore";
import { DRAFT_WORKSPACE_COLUMN_MAX } from "./types";

export { DRAFT_WORKSPACE_COLUMN_MAX };

export const DRAFT_WORKSPACE_PREFERENCES_SCHEMA_VERSION = 3 as const;
export const DRAFT_WORKSPACE_BOARD_BREAKPOINT_PX = 1024;
export const DRAFT_WORKSPACE_COLUMN_MIN = 2;
export const DRAFT_WORKSPACE_PACK_SCALE_DEFAULT = 1.65;
export const DRAFT_WORKSPACE_PACK_SCALE_MIN = 0.4;
export const DRAFT_WORKSPACE_PACK_SCALE_MAX = 2.9;
export const DRAFT_WORKSPACE_PACK_SCALE_STEP = 0.01;
export const DRAFT_PACK_CARD_BASE_WIDTH_PX = 146;
export const DRAFT_WORKSPACE_COLLAPSED_SIDEBOARD_CARD_WIDTH_PX
  = DRAFT_PACK_CARD_BASE_WIDTH_PX * DRAFT_WORKSPACE_PACK_SCALE_DEFAULT;

export type DraftWorkspaceView = "board" | "compact";
export type ResponsiveDraftLayout =
  | "phone-portrait"
  | "phone-landscape"
  | "tablet-portrait"
  | "tablet-landscape"
  | "desktop";
export type DraftBoardSort = "cmc" | "color" | "rarity" | "type";
export type DraftBoardRows = "one" | "two";
export type DraftCardPreviewMode = "none" | CardPreviewMode;

export interface DraftBoardPreferences {
  sort: DraftBoardSort;
  columnCount: number;
  rows: DraftBoardRows;
  showHeaders: boolean;
}

export interface DraftPhoneDeckVisualColumnCaps {
  portrait: number;
  landscape: number;
}

export interface DraftTabletDeckVisualColumnCaps {
  portrait: number;
  landscape: number;
}

export interface DraftWorkspacePreferences {
  schemaVersion: typeof DRAFT_WORKSPACE_PREFERENCES_SCHEMA_VERSION;
  explicitView: DraftWorkspaceView | null;
  cardPreviewMode: DraftCardPreviewMode;
  packScale: number;
  sideboardCollapsed: boolean | null;
  builderPhoneSideboardCollapsed: boolean;
  phoneDeckVisualColumnCaps: DraftPhoneDeckVisualColumnCaps;
  tabletDeckVisualColumnCaps: DraftTabletDeckVisualColumnCaps;
  deck: DraftBoardPreferences;
  sideboard: DraftBoardPreferences;
}

const DECK_DEFAULTS: Readonly<DraftBoardPreferences> = {
  sort: "cmc",
  columnCount: 7,
  rows: "one",
  showHeaders: true,
};

const SIDEBOARD_DEFAULTS: Readonly<DraftBoardPreferences> = {
  sort: "cmc",
  columnCount: 6,
  rows: "one",
  showHeaders: true,
};

const PHONE_DECK_VISUAL_COLUMN_CAPS_DEFAULTS: Readonly<DraftPhoneDeckVisualColumnCaps> = {
  portrait: 3,
  landscape: 5,
};

const TABLET_DECK_VISUAL_COLUMN_CAPS_DEFAULTS: Readonly<DraftTabletDeckVisualColumnCaps> = {
  portrait: 3,
  landscape: 5,
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isDraftWorkspaceView(value: unknown): value is DraftWorkspaceView {
  return value === "board" || value === "compact";
}

function isDraftCardPreviewMode(value: unknown): value is DraftCardPreviewMode {
  return value === "none" || value === "follow" || value === "side" || value === "shift";
}

function isDraftBoardSort(value: unknown): value is DraftBoardSort {
  return value === "cmc" || value === "color" || value === "rarity" || value === "type";
}

function isDraftBoardRows(value: unknown): value is DraftBoardRows {
  return value === "one" || value === "two";
}

function clampColumnCount(value: unknown, fallback: number): number {
  if (typeof value !== "number" || !Number.isFinite(value) || !Number.isInteger(value)) {
    return fallback;
  }
  return Math.min(DRAFT_WORKSPACE_COLUMN_MAX, Math.max(DRAFT_WORKSPACE_COLUMN_MIN, value));
}

export function repairDraftWorkspacePackScale(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return DRAFT_WORKSPACE_PACK_SCALE_DEFAULT;
  }
  const hundredths = Math.round(value * 100);
  return Math.min(
    DRAFT_WORKSPACE_PACK_SCALE_MAX * 100,
    Math.max(DRAFT_WORKSPACE_PACK_SCALE_MIN * 100, hundredths),
  ) / 100;
}

function repairBoardPreferences(
  value: unknown,
  defaults: Readonly<DraftBoardPreferences>,
): DraftBoardPreferences {
  if (!isRecord(value)) return { ...defaults };
  return {
    sort: isDraftBoardSort(value.sort) ? value.sort : defaults.sort,
    columnCount: clampColumnCount(value.columnCount, defaults.columnCount),
    rows: isDraftBoardRows(value.rows) ? value.rows : defaults.rows,
    showHeaders: typeof value.showHeaders === "boolean" ? value.showHeaders : defaults.showHeaders,
  };
}

function repairPhoneDeckVisualColumnCaps(value: unknown): DraftPhoneDeckVisualColumnCaps {
  if (!isRecord(value)) return { ...PHONE_DECK_VISUAL_COLUMN_CAPS_DEFAULTS };
  return {
    portrait: clampPhoneDeckVisualColumnCap(value.portrait, PHONE_DECK_VISUAL_COLUMN_CAPS_DEFAULTS.portrait),
    landscape: clampPhoneDeckVisualColumnCap(value.landscape, PHONE_DECK_VISUAL_COLUMN_CAPS_DEFAULTS.landscape),
  };
}

function clampPhoneDeckVisualColumnCap(value: unknown, fallback: number): number {
  if (typeof value !== "number" || !Number.isFinite(value) || !Number.isInteger(value)) {
    return fallback;
  }
  return Math.min(5, Math.max(1, value));
}

function repairTabletDeckVisualColumnCaps(value: unknown): DraftTabletDeckVisualColumnCaps {
  if (!isRecord(value)) return { ...TABLET_DECK_VISUAL_COLUMN_CAPS_DEFAULTS };
  return {
    portrait: clampTabletDeckVisualColumnCap(value.portrait, TABLET_DECK_VISUAL_COLUMN_CAPS_DEFAULTS.portrait),
    landscape: clampTabletDeckVisualColumnCap(value.landscape, TABLET_DECK_VISUAL_COLUMN_CAPS_DEFAULTS.landscape),
  };
}

function clampTabletDeckVisualColumnCap(value: unknown, fallback: number): number {
  if (typeof value !== "number" || !Number.isFinite(value) || !Number.isInteger(value)) {
    return fallback;
  }
  return Math.min(15, Math.max(1, value));
}

function repairSchemaV1Preferences(value: Record<string, unknown>): DraftWorkspacePreferences {
  return {
    schemaVersion: DRAFT_WORKSPACE_PREFERENCES_SCHEMA_VERSION,
    explicitView: value.explicitView === null || isDraftWorkspaceView(value.explicitView)
      ? value.explicitView
      : null,
    cardPreviewMode: isDraftCardPreviewMode(value.cardPreviewMode) ? value.cardPreviewMode : "none",
    packScale: repairDraftWorkspacePackScale(value.packScale),
    sideboardCollapsed: value.sideboardCollapsed === null || typeof value.sideboardCollapsed === "boolean"
      ? value.sideboardCollapsed
      : null,
    builderPhoneSideboardCollapsed: true,
    phoneDeckVisualColumnCaps: { ...PHONE_DECK_VISUAL_COLUMN_CAPS_DEFAULTS },
    tabletDeckVisualColumnCaps: { ...TABLET_DECK_VISUAL_COLUMN_CAPS_DEFAULTS },
    deck: repairBoardPreferences(value.deck, DECK_DEFAULTS),
    sideboard: repairBoardPreferences(value.sideboard, SIDEBOARD_DEFAULTS),
  };
}

export function createDefaultDraftWorkspacePreferences(): DraftWorkspacePreferences {
  return {
    schemaVersion: DRAFT_WORKSPACE_PREFERENCES_SCHEMA_VERSION,
    explicitView: null,
    cardPreviewMode: "none",
    packScale: DRAFT_WORKSPACE_PACK_SCALE_DEFAULT,
    sideboardCollapsed: null,
    builderPhoneSideboardCollapsed: true,
    phoneDeckVisualColumnCaps: { ...PHONE_DECK_VISUAL_COLUMN_CAPS_DEFAULTS },
    tabletDeckVisualColumnCaps: { ...TABLET_DECK_VISUAL_COLUMN_CAPS_DEFAULTS },
    deck: { ...DECK_DEFAULTS },
    sideboard: { ...SIDEBOARD_DEFAULTS },
  };
}

export function repairDraftWorkspacePreferences(value: unknown): DraftWorkspacePreferences {
  if (!isRecord(value)) {
    return createDefaultDraftWorkspacePreferences();
  }
  if (value.schemaVersion === 1) return repairSchemaV1Preferences(value);
  if (value.schemaVersion === 2) {
    const phoneDeckVisualColumnCaps = repairPhoneDeckVisualColumnCaps(value.phoneDeckVisualColumnCaps);
    return {
      schemaVersion: DRAFT_WORKSPACE_PREFERENCES_SCHEMA_VERSION,
      explicitView: value.explicitView === null || isDraftWorkspaceView(value.explicitView)
        ? value.explicitView
        : null,
      cardPreviewMode: isDraftCardPreviewMode(value.cardPreviewMode) ? value.cardPreviewMode : "none",
      packScale: repairDraftWorkspacePackScale(value.packScale),
      sideboardCollapsed: value.sideboardCollapsed === null || typeof value.sideboardCollapsed === "boolean"
        ? value.sideboardCollapsed
        : null,
      builderPhoneSideboardCollapsed: typeof value.builderPhoneSideboardCollapsed === "boolean"
        ? value.builderPhoneSideboardCollapsed
        : true,
      phoneDeckVisualColumnCaps,
      tabletDeckVisualColumnCaps: { ...phoneDeckVisualColumnCaps },
      deck: repairBoardPreferences(value.deck, DECK_DEFAULTS),
      sideboard: repairBoardPreferences(value.sideboard, SIDEBOARD_DEFAULTS),
    };
  }
  if (value.schemaVersion !== DRAFT_WORKSPACE_PREFERENCES_SCHEMA_VERSION) {
    return createDefaultDraftWorkspacePreferences();
  }

  return {
    schemaVersion: DRAFT_WORKSPACE_PREFERENCES_SCHEMA_VERSION,
    explicitView: value.explicitView === null || isDraftWorkspaceView(value.explicitView)
      ? value.explicitView
      : null,
    cardPreviewMode: isDraftCardPreviewMode(value.cardPreviewMode) ? value.cardPreviewMode : "none",
    packScale: repairDraftWorkspacePackScale(value.packScale),
    sideboardCollapsed: value.sideboardCollapsed === null || typeof value.sideboardCollapsed === "boolean"
      ? value.sideboardCollapsed
      : null,
    builderPhoneSideboardCollapsed: typeof value.builderPhoneSideboardCollapsed === "boolean"
      ? value.builderPhoneSideboardCollapsed
      : true,
    phoneDeckVisualColumnCaps: repairPhoneDeckVisualColumnCaps(value.phoneDeckVisualColumnCaps),
    tabletDeckVisualColumnCaps: repairTabletDeckVisualColumnCaps(value.tabletDeckVisualColumnCaps),
    deck: repairBoardPreferences(value.deck, DECK_DEFAULTS),
    sideboard: repairBoardPreferences(value.sideboard, SIDEBOARD_DEFAULTS),
  };
}

export function loadDraftWorkspacePreferences(): DraftWorkspacePreferences {
  try {
    const raw = localStorage.getItem(DRAFT_WORKSPACE_PREFERENCES_KEY);
    return raw === null
      ? createDefaultDraftWorkspacePreferences()
      : repairDraftWorkspacePreferences(JSON.parse(raw));
  } catch {
    return createDefaultDraftWorkspacePreferences();
  }
}

export function saveDraftWorkspacePreferences(
  value: DraftWorkspacePreferences,
): "saved" | "storage-unavailable" {
  try {
    localStorage.setItem(
      DRAFT_WORKSPACE_PREFERENCES_KEY,
      JSON.stringify(repairDraftWorkspacePreferences(value)),
    );
    return "saved";
  } catch {
    return "storage-unavailable";
  }
}

export function resolveDraftWorkspaceView(
  explicitView: DraftWorkspaceView | null,
  viewportWidth: number,
  responsiveLayout?: ResponsiveDraftLayout,
): DraftWorkspaceView {
  if (explicitView !== null) return explicitView;
  if (responsiveLayout !== undefined && responsiveLayout !== "desktop") return "compact";
  return viewportWidth >= DRAFT_WORKSPACE_BOARD_BREAKPOINT_PX ? "board" : "compact";
}

export function getResponsiveDraftLayout(
  viewportWidth: number,
  viewportHeight: number,
): ResponsiveDraftLayout {
  if (viewportWidth >= 1200) return "desktop";
  if (viewportWidth > viewportHeight && viewportHeight < 600) return "phone-landscape";
  if (viewportWidth < 640) return "phone-portrait";
  if (viewportWidth < 900) {
    return viewportWidth > viewportHeight ? "phone-landscape" : "tablet-portrait";
  }
  return viewportWidth > viewportHeight ? "tablet-landscape" : "tablet-portrait";
}

export function resolveDraftWorkspaceVisualColumnCap(
  responsiveLayout: ResponsiveDraftLayout,
  responsiveContext: "draft" | "builder",
  phoneDeckVisualColumnCaps: DraftPhoneDeckVisualColumnCaps,
  tabletDeckVisualColumnCaps: DraftTabletDeckVisualColumnCaps,
): number | undefined {
  if (responsiveLayout === "phone-portrait") {
    return phoneDeckVisualColumnCaps.portrait;
  }
  if (responsiveLayout === "phone-landscape") {
    return phoneDeckVisualColumnCaps.landscape;
  }
  if (responsiveLayout === "tablet-portrait") {
    return responsiveContext === "builder"
      ? tabletDeckVisualColumnCaps.portrait
      : phoneDeckVisualColumnCaps.portrait;
  }
  if (responsiveLayout === "tablet-landscape") {
    return responsiveContext === "builder"
      ? tabletDeckVisualColumnCaps.landscape
      : phoneDeckVisualColumnCaps.landscape;
  }
  return undefined;
}

export function resolveDraftWorkspaceSideboardCollapsed(
  explicitValue: boolean | null,
  _viewportWidth: number,
  responsiveLayout?: ResponsiveDraftLayout,
  responsiveContext: "draft" | "builder" = "draft",
  builderPhoneSideboardCollapsed = true,
): boolean {
  if (responsiveContext === "builder" && (responsiveLayout === "phone-portrait" || responsiveLayout === "phone-landscape")) {
    return builderPhoneSideboardCollapsed;
  }
  if (responsiveContext === "draft" && responsiveLayout === "phone-portrait") {
    return explicitValue ?? true;
  }
  if (responsiveContext === "draft" && responsiveLayout === "phone-landscape") {
    return explicitValue ?? false;
  }
  if (responsiveLayout === "tablet-portrait" || responsiveLayout === "tablet-landscape") {
    return explicitValue ?? true;
  }
  if (responsiveLayout !== undefined && responsiveLayout !== "desktop") return true;
  if (explicitValue !== null) return explicitValue;
  return true;
}
