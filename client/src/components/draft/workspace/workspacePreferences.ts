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
/**
 * Pile card scale for a shared-stack draft, on the same axis and the same base
 * width as `packScale` but stored separately.
 *
 * A separate number rather than a reuse, because the two surfaces show
 * different things at once: a pack is a grid of up to fifteen cards, while a
 * Winston pile row is one pile's revealed run beside two face-down stacks. A
 * player who has tuned one has said nothing about the other, and a shared
 * value would move a setting they never touched the first time they draft the
 * other format.
 *
 * The default is larger than `packScale`'s because the surface is one row wide
 * rather than a filled grid — the cards being decided on get the whole width.
 */
export const DRAFT_WORKSPACE_PILE_SCALE_DEFAULT = 1.35;
export const DRAFT_WORKSPACE_PILE_SCALE_MIN = 0.4;
export const DRAFT_WORKSPACE_PILE_SCALE_MAX = 2.9;
export const DRAFT_WORKSPACE_PILE_SCALE_STEP = 0.01;
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
  /** Shared-stack pile card scale. See `DRAFT_WORKSPACE_PILE_SCALE_DEFAULT`. */
  pileScale: number;
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

/**
 * The pile-scale half of the same repair, and the reason the schema version did
 * NOT move for this field: every stored-shape branch below runs this, and it is
 * total — a preferences blob written before `pileScale` existed has `undefined`
 * here and gets the default, which is exactly the migration the field needs. A
 * version bump would instead send every existing v3 blob down the
 * unknown-version arm and discard the player's pack scale, columns and sort
 * along with it.
 */
export function repairDraftWorkspacePileScale(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return DRAFT_WORKSPACE_PILE_SCALE_DEFAULT;
  }
  const hundredths = Math.round(value * 100);
  return Math.min(
    DRAFT_WORKSPACE_PILE_SCALE_MAX * 100,
    Math.max(DRAFT_WORKSPACE_PILE_SCALE_MIN * 100, hundredths),
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
  return Math.min(10, Math.max(1, value));
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
  return Math.min(10, Math.max(1, value));
}

function repairSchemaV1Preferences(value: Record<string, unknown>): DraftWorkspacePreferences {
  return {
    schemaVersion: DRAFT_WORKSPACE_PREFERENCES_SCHEMA_VERSION,
    explicitView: value.explicitView === null || isDraftWorkspaceView(value.explicitView)
      ? value.explicitView
      : null,
    cardPreviewMode: isDraftCardPreviewMode(value.cardPreviewMode) ? value.cardPreviewMode : "none",
    packScale: repairDraftWorkspacePackScale(value.packScale),
    pileScale: repairDraftWorkspacePileScale(value.pileScale),
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
    pileScale: DRAFT_WORKSPACE_PILE_SCALE_DEFAULT,
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
      pileScale: repairDraftWorkspacePileScale(value.pileScale),
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
    pileScale: repairDraftWorkspacePileScale(value.pileScale),
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

/**
 * The deck board geometry the mounted draft page is currently showing.
 *
 * Read by the stores' arriving-card placement, which has to know which columns
 * the board means but has no access to page state. It lives here rather than in
 * either store because it is a PRESENTATION preference — the same thing
 * `loadDraftWorkspacePreferences` reads and `saveDraftWorkspacePreferences`
 * writes — and because both `draftStore` and `multiplayerDraftStore` need it:
 * a copy per store would be two same-named exports a caller could import from
 * the wrong module.
 *
 * Module state rather than store state: no view publishes it, and it is absent
 * from the persisted `QuickDraftSnapshotInput`.
 *
 * Seeded from the player's STORED preferences rather than the module defaults,
 * so an install that lands before the page has published anything still places
 * against the columns the player chose. Pinned by
 * `draftStore.workspace.test.ts::places_an_install_before_any_publish_against_the_stored_preferences`,
 * which seeds `localStorage`, re-imports the store into a fresh module registry
 * and drives a `startDraft` install with no publish in front of it; making this
 * initializer `{ ...DECK_DEFAULTS }` reds it with `expected 6 to be 2`.
 *
 * One page is mounted at a time — `/draft/quick` and `/draft-pod` are separate
 * routes — so the solo and pod pages never contend. The publishers are
 * `DraftPage`'s `handleWorkspacePreferencesChange` and the DRAFTING screen's
 * `handlePreferencesChange` in `DraftPodPage`, each with a mount effect
 * alongside it. `DraftPodPage` has two further same-named handlers, on the
 * intergame screen and in `PodDeckBuilder`, which save without publishing:
 * neither screen receives pool arrivals, and the drafting screen's mount effect
 * re-reads `localStorage` on the way back in.
 */
let arrivingCardBoardPreferences: DraftBoardPreferences =
  loadDraftWorkspacePreferences().deck;

/** Tell the placement paths which columns the deck board currently means. */
export function setArrivingCardBoardPreferences(preferences: DraftBoardPreferences): void {
  arrivingCardBoardPreferences = preferences;
}

/** The value last published by the mounted draft page. */
export function getArrivingCardBoardPreferences(): DraftBoardPreferences {
  return arrivingCardBoardPreferences;
}

export function resolveDraftWorkspaceView(
  explicitView: DraftWorkspaceView | null,
  viewportWidth: number,
  responsiveLayout?: ResponsiveDraftLayout,
): DraftWorkspaceView {
  if (responsiveLayout === "desktop") return "board";
  if (explicitView !== null) return explicitView;
  if (responsiveLayout !== undefined) return "compact";
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
    return explicitValue ?? true;
  }
  if (responsiveLayout === "tablet-portrait" || responsiveLayout === "tablet-landscape") {
    return explicitValue ?? true;
  }
  if (responsiveLayout !== undefined && responsiveLayout !== "desktop") return true;
  if (explicitValue !== null) return explicitValue;
  return true;
}
