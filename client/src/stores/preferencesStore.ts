import { create } from "zustand";
import { persist } from "zustand/middleware";

import type {
  GameFormat,
  MatchType,
  PhaseStop,
  PriorityPassingMode,
} from "../adapter/types";
import type { CommanderBracket } from "../types/bracket";
import type { SortKey } from "../components/modal/cardChoice/gridSelection";
import {
  ANIMATION_SPEED_DEFAULT,
  ANIMATION_SPEED_MAX,
  ANIMATION_SPEED_MIN,
  PACING_DEFAULT,
  PACING_MAX,
  PACING_MIN,
  defaultPacingMultipliers,
  type PacingCategory,
  type VfxQuality,
} from "../animation/types";
import type { AIDifficulty } from "../constants/ai";
import { DEFAULT_AI_DIFFICULTY } from "../constants/ai";
import type { DeckArchetype } from "../services/engineRuntime";
import { detectInitialLanguage, SUPPORTED_LNGS, type SupportedLng } from "../i18n/resources";

/** Literal sentinel for "any deck" in AI deck selection. Mirrors `DeckChoice::Random`
 *  naming so the preference value is self-describing without a nullable field. */
export const AI_DECK_RANDOM = "Random" as const;
export type AiDeckSelection = typeof AI_DECK_RANDOM | string;
export type AiArchetypeFilter = "Any" | DeckArchetype;
export const DEFAULT_AI_COVERAGE_FLOOR = 90;

/** Per-seat AI preferences. Index 0 = first AI opponent. The `aiSeats` array
 *  grows to `playerCount - 1` via `ensureAiSeatCount` whenever the user changes
 *  the player count slider. Archetype and coverage filters remain global: they
 *  filter the *pool* of Random picks, a concept that doesn't vary per seat. */
export interface AiSeatPref {
  difficulty: AIDifficulty;
  deckId: AiDeckSelection;
}

export type ArtChainEntry =
  | { type: "set"; setCode: string; label: string }
  | { type: "newest" }
  | { type: "oldest" }
  | { type: "prefer_borderless" }
  | { type: "prefer_extended" }
  | { type: "source_printing" };

export interface CardArtOverride {
  scryfallId: string;
  setCode: string;
  collectorNumber: string;
}

export type CardSizePreference = "small" | "medium" | "large";
/** How the hover card-preview behaves on desktop.
 *  "follow" = the preview tracks the cursor (prior fixed behavior, default).
 *  "side"   = the preview docks to the screen edge so it never covers the board.
 *  "shift"  = the preview only appears while the Shift key is held (Tabletop
 *             Simulator style), letting the player read the board uninterrupted. */
export type CardPreviewMode = "follow" | "side" | "shift";
/** Card-preview hover latency bounds (milliseconds). `0` = instant (the
 *  default — the preview appears the moment the cursor lands on a card). The
 *  upper bound keeps the slider meaningful; a delay longer than ~1s defeats the
 *  purpose of an at-a-glance preview. Only applies to the hover-driven preview
 *  modes ("follow"/"side"); the "shift" bind-key mode shows immediately on
 *  keypress, so the latency is mutually exclusive with it. */
export const CARD_PREVIEW_HOVER_DELAY_MIN = 0;
export const CARD_PREVIEW_HOVER_DELAY_MAX = 1000;
export const CARD_PREVIEW_HOVER_DELAY_STEP = 50;
export type HudLayout = "inline" | "floating";
export type LogDefaultState = "open" | "closed";
export type BattlefieldCardDisplay = "art_crop" | "full_card";
/** How the command zone (commander card, tax, emblems, commander damage) is laid
 *  out. "inline" = a bounded always-visible corner dock; "compact" = a collapsed
 *  pile that expands to a popover on hover; "auto" = pick by viewport at use-site
 *  (compact on short/narrow screens, inline otherwise). Resolved via
 *  {@link useResolvedCommandZoneDisplay}, mirroring the `boardBackground`
 *  "auto-wubrg" resolve-at-use-site precedent. */
export type CommandZoneDisplay = "compact" | "inline" | "auto";
/** Whether a battlefield sub-row (lands / support) collapses into its summary
 *  tile. "auto" = collapse once the row exceeds the crowding threshold (the
 *  prior fixed behavior); "on" = always collapse into the tile; "off" = never
 *  collapse (always show the full row). Resolved at the use-site in
 *  {@link BattlefieldZoneOverflow}, mirroring the `commandZoneDisplay`
 *  tri-state "auto" precedent. Lands and support each carry their own value. */
export type ZoneCollapseMode = "auto" | "on" | "off";
export type TapRotation = "mtga" | "classic";
export type SpellPaymentMode = "auto" | "manual";
/** Which screen edge the resolving-stack panel docks to (and collapses toward).
 *  User-chosen so a player can keep the stack off whichever side of the
 *  battlefield they care about — e.g. dock left to free the right action rail. */
export type StackDockSide = "left" | "right";
/** Opponent HUD density in the multi-opponent rail. "comfortable" = the full
 *  two-row tab (name + life over the board-composition breakdown); "compact" =
 *  a single thin row (small avatar + name + life) that trades the breakdown for
 *  vertical real-estate. Player-toggleable from the rail. */
export type OpponentHudDensity = "comfortable" | "compact";
export type MultiplayerBoardLayout = "focused" | "split";
/** "auto-wubrg" picks a random battlefield matching the dominant mana color.
 *  "random" picks a random battlefield each game regardless of color.
 *  "none" disables the background image.
 *  "custom" uses the URL stored in `customBackgroundUrl`.
 *  Any other string is a battlefield or plain-color ID. */
export type BoardBackground = "auto-wubrg" | "random" | "none" | "custom" | (string & {});

// ── Flex layout ──────────────────────────────────────────────────────────────
/** A pixel delta from a widget's docked default position. The *absence* of an
 *  entry means the widget sits exactly where it does today, so a fresh store and
 *  every legacy store render byte-identically (zero-regression default). */
export interface WidgetOffset {
  dx: number;
  dy: number;
}
/** Draggable widgets whose position is shared across all table sizes. The
 *  opponent HUD is intentionally absent here — it's the one element whose
 *  structure differs by table size, so it's keyed separately (see
 *  {@link FlexTableSize}). */
export type FlexWidgetKey =
  | "playerHud"
  | "stackPanel"
  | "logPanel"
  | "actionRail"
  | "playerPiles"
  | "opponentPiles";
/** The three reorderable cells of the battlefield middle row. Stored as an
 *  order so the user can permute them (drag-to-reorder); flexbox reflows. */
export type MiddleCell = "lands" | "support" | "command";
/** Default left-to-right order — reproduces today's lands · support · command. */
export const DEFAULT_MIDDLE_ROW_ORDER: readonly MiddleCell[] = ["lands", "support", "command"];
/** Table sizes the opponent HUD position is keyed by: 1v1 renders a single pill,
 *  multiplayer a tab strip, so a shared offset wouldn't fit both. */
export type FlexTableSize = "oneVsOne" | "multiplayer";
/** A board grid row capped at `min(pct%, pxCap px)` — mirrors today's
 *  `minmax(0,min(12%,100px))` track. The middle (battlefield) row is always
 *  `1fr` and absorbs the remainder, so only the top/bottom bands are stored. */
export interface CappedTrack {
  pct: number;
  pxCap: number;
}
export interface GridBands {
  top: CappedTrack;
  bottom: CappedTrack;
}
/** Preset ids are intentionally neutral ("Layout N"): the alternative layouts
 *  carry no principled design rationale, so naming them by use-case ("Streamer")
 *  would overclaim. `default` is load-bearing — it is the {@link defaultFlexLayout}
 *  seed and the Reset target — so only the two editorial slots are neutralized. */
export type FlexPresetId = "default" | "layout2" | "layout3" | "custom";
/** An aspect-preserving size multiplier. Two flavours share one map:
 *  content-scales — `stack` (the stack's cards, over the viewport
 *  `responsiveScale`) and `summaryTile` (the collapsed lands/support overflow
 *  pills) — and widget box-scales — `actionRail` and `playerPiles` — applied as
 *  a `transform: scale()` on the whole `DraggableWidget`. Absent ⇒ 1. */
export type FlexScaleKey = "stack" | "summaryTile" | "actionRail" | "playerPiles";
/** Content alignment within a middle-row cell — maps to flexbox `justify-*`. */
export type CellAlign = "start" | "center" | "end";
/** Per-cell default alignment, reproducing the prior hardcoded layout: lands hug
 *  the left, support the right, command centered. Absent key ⇒ this. */
export const DEFAULT_CELL_ALIGN: Record<MiddleCell, CellAlign> = {
  lands: "start",
  support: "end",
  command: "center",
};
/** Persisted board layout. One shared global config; only the opponent HUD is
 *  table-size-keyed. Presets are authoritative — applying one replaces every
 *  field wholesale. Any manual edit flips `activePreset` to "custom".
 *
 *  `landSupportRatio` and `scales` are optional so a config persisted before
 *  they existed (or cloud-synced from an older client) reads as the neutral
 *  default rather than `undefined`; consumers apply `?? 0.5` / `?? 1`. */
export interface FlexLayoutConfig {
  gridBands: GridBands;
  /** Lands' share of the lands↔support middle row, 0..1. Support takes the
   *  remainder (`1 - ratio`). Absent ⇒ 0.5 (the prior equal `flex-1` split). */
  landSupportRatio?: number;
  /** Left-to-right order of the middle-row cells. Absent ⇒
   *  {@link DEFAULT_MIDDLE_ROW_ORDER} (lands · support · command). */
  middleRowOrder?: MiddleCell[];
  /** Per-zone aspect-preserving size multipliers. Absent key ⇒ 1.0. */
  scales?: Partial<Record<FlexScaleKey, number>>;
  /** Per-cell content alignment. Absent key ⇒ {@link DEFAULT_CELL_ALIGN}. */
  cellAlign?: Partial<Record<MiddleCell, CellAlign>>;
  widgets: Partial<Record<FlexWidgetKey, WidgetOffset>>;
  opponentHudByTableSize: Partial<Record<FlexTableSize, WidgetOffset>>;
  activePreset: FlexPresetId;
}

/** Neutral default for the lands↔support split — equal halves, matching the
 *  prior hardcoded `flex-1` / `flex-1`. */
export const DEFAULT_LAND_SUPPORT_RATIO = 0.5;

/** Factory for the default layout. This IS the "default" preset baseline and the
 *  reset target; `presets.ts` imports it rather than redefining it. The band
 *  values reproduce today's desktop `gridTemplateRows` exactly (see
 *  {@link useResolvedGridRows}). Returned as a function so nested objects are
 *  never shared between the store and the defaults snapshot. */
export function defaultFlexLayout(): FlexLayoutConfig {
  return {
    gridBands: { top: { pct: 12, pxCap: 100 }, bottom: { pct: 18, pxCap: 150 } },
    landSupportRatio: DEFAULT_LAND_SUPPORT_RATIO,
    middleRowOrder: [...DEFAULT_MIDDLE_ROW_ORDER],
    scales: {},
    cellAlign: {},
    widgets: {},
    opponentHudByTableSize: {},
    activePreset: "default",
  };
}

/** Deep-clone a layout config so applying a preset constant can never let a
 *  later in-store mutation corrupt the shared preset object. */
function cloneFlexLayout(config: FlexLayoutConfig): FlexLayoutConfig {
  return {
    gridBands: {
      top: { ...config.gridBands.top },
      bottom: { ...config.gridBands.bottom },
    },
    landSupportRatio: config.landSupportRatio,
    middleRowOrder: config.middleRowOrder ? [...config.middleRowOrder] : undefined,
    scales: { ...config.scales },
    cellAlign: { ...config.cellAlign },
    widgets: Object.fromEntries(
      Object.entries(config.widgets).map(([k, v]) => [k, { ...v }]),
    ),
    opponentHudByTableSize: Object.fromEntries(
      Object.entries(config.opponentHudByTableSize).map(([k, v]) => [k, { ...v }]),
    ),
    activePreset: config.activePreset,
  };
}

function defaultAiSeat(): AiSeatPref {
  return { difficulty: DEFAULT_AI_DIFFICULTY, deckId: AI_DECK_RANDOM };
}

function clamp(value: number, min: number, max: number): number {
  if (Number.isNaN(value)) return min;
  return Math.min(Math.max(value, min), max);
}

/** Map the v1 AnimationSpeed enum to its numeric multiplier. Used by the
 *  v1→v2 persist migration so existing users don't lose their setting. */
const LEGACY_ANIMATION_SPEED_MULTIPLIERS: Record<string, number> = {
  slow: 1.5,
  normal: 1.0,
  fast: 0.5,
  instant: 0,
};

/** Map the v1 CombatPacing enum to its numeric multiplier. */
const LEGACY_COMBAT_PACING_MULTIPLIERS: Record<string, number> = {
  normal: 1.0,
  slow: 1.35,
  cinematic: 1.75,
};

/** Factory returning a freshly-allocated default preferences snapshot. Returned
 *  as a function (not a const) because the state contains nested objects
 *  (`pacingMultipliers`, `aiSeats`, `customThemeUrls`, `phaseStops`) — sharing
 *  those references between the store and the "defaults" snapshot would let
 *  setters silently mutate the defaults. Used by both store init and
 *  `resetAllPreferences`, so they can never drift apart. */
function buildDefaultPreferences(): PreferencesState {
  return {
    language: detectInitialLanguage(),
    cardSize: "medium",
    hudLayout: "inline",
    followActiveOpponent: true,
    logDefaultState: "closed",
    boardBackground: "auto-wubrg",
    customBackgroundUrl: "",
    vfxQuality: "full",
    animationSpeedMultiplier: ANIMATION_SPEED_DEFAULT,
    pacingMultipliers: defaultPacingMultipliers(),
    phaseStops: [],
    priorityPassingMode: "Standard",
    masterVolume: 100,
    sfxVolume: 70,
    musicVolume: 40,
    sfxMuted: false,
    musicMuted: false,
    masterMuted: false,
    audioThemeId: "planeswalker",
    customThemeUrls: [],
    battlefieldCardDisplay: "art_crop",
    collapsedFolderIds: [],
    lastSeenChangelogId: undefined,
    commandZoneDisplay: "auto",
    collapseLands: "auto",
    collapseSupport: "auto",
    tapRotation: "mtga",
    spellPaymentMode: "auto",
    handSort: "none",
    showKeywordStrip: true,
    battlefieldPeekOnHover: true,
    cardPreviewMode: "follow",
    cardPreviewHoverDelayMs: 0,
    showCardPreviewFooter: true,
    stackDockSide: "right",
    opponentHudDensity: "comfortable",
    multiplayerBoardLayout: "focused",
    aiSeats: [defaultAiSeat()],
    cedhMode: false,
    aiArchetypeFilter: "Any",
    aiCoverageFloor: DEFAULT_AI_COVERAGE_FLOOR,
    aiBracketFilter: [] as CommanderBracket[],
    lastFormat: null,
    lastMatchType: "Bo1",
    lastPlayerCount: 2,
    dismissedFlowHelpNudge: false,
    dismissedSandboxToolsNudge: false,
    dismissedReportCardNudge: false,
    artChain: [] as ArtChainEntry[],
    artOverrides: {} as Record<string, CardArtOverride>,
    flexLayout: defaultFlexLayout(),
    telemetryEnabled: true,
    nativeEngineEnabled: true,
  };
}

interface PreferencesState {
  /** Active UI language. Drives react-i18next chrome translation (the store is the
   *  single source of truth; `i18n/index.ts` mirrors changes). Closed union — not
   *  the open `(string & {})` pattern, since the supported set is fixed. */
  language: SupportedLng;
  cardSize: CardSizePreference;
  hudLayout: HudLayout;
  followActiveOpponent: boolean;
  logDefaultState: LogDefaultState;
  boardBackground: BoardBackground;
  customBackgroundUrl: string;
  vfxQuality: VfxQuality;
  /** Continuous global animation-speed multiplier. `0` = instant (skip waits).
   *  `1` = neutral. Higher = slower playback. Multiplies every per-category
   *  duration after pacingMultipliers is applied. */
  animationSpeedMultiplier: number;
  /** Per-category pacing multipliers — see `PacingCategory` in animation/types
   *  for the full list. Each event's category is resolved via `eventCategory()`
   *  and the matching multiplier scales its base duration. */
  pacingMultipliers: Record<PacingCategory, number>;
  phaseStops: PhaseStop[];
  priorityPassingMode: PriorityPassingMode;
  masterVolume: number;
  sfxVolume: number;
  musicVolume: number;
  sfxMuted: boolean;
  musicMuted: boolean;
  masterMuted: boolean;
  audioThemeId: string;
  customThemeUrls: Array<{ id: string; url: string }>;
  battlefieldCardDisplay: BattlefieldCardDisplay;
  /** Ids of deck-library folders the user has collapsed (id present = collapsed).
   * Also holds the sentinel ids for the virtual Starred/Unfiled sections. */
  collapsedFolderIds: string[];
  /** Highest changelog entry id the user has seen ("What's New" watermark).
   * Undefined for first-run / freshly-upgraded users — the changelog hook
   * silently seeds it to the current latest so they get no unread dot for
   * entries that predate their first visit. */
  lastSeenChangelogId?: number;
  /** Command-zone layout mode (inline dock / compact pile / auto-by-viewport). */
  commandZoneDisplay: CommandZoneDisplay;
  /** Whether the lands sub-row collapses into its summary tile (auto/on/off). */
  collapseLands: ZoneCollapseMode;
  /** Whether the support sub-row collapses into its summary tile (auto/on/off). */
  collapseSupport: ZoneCollapseMode;
  tapRotation: TapRotation;
  spellPaymentMode: SpellPaymentMode;
  /** Persisted sort order for the player's own hand (display-only — never
   *  reorders `player.hand`). Mirrors the discard grid's `SortKey`; defaults to
   *  "none" (insertion order, the prior behavior). The hide-filter is kept
   *  ephemeral per-game in `uiStore.handFilter`. */
  handSort: SortKey;
  showKeywordStrip: boolean;
  /** When true, hovering an unfocused opponent's tab opens a small popover
   *  previewing that opponent's nonland permanents. Disable for a quieter
   *  HUD — focus is still reachable via tab click. */
  battlefieldPeekOnHover: boolean;
  /** Desktop hover card-preview behavior — follow cursor, dock to the side, or
   *  only show while Shift is held. See {@link CardPreviewMode}. */
  cardPreviewMode: CardPreviewMode;
  /** Latency (ms) before the hover preview appears in the "follow"/"side"
   *  modes. `0` = instant (default). Ignored in "shift" mode, which is
   *  keypress-triggered. See {@link CARD_PREVIEW_HOVER_DELAY_MAX}. */
  cardPreviewHoverDelayMs: number;
  /** Whether desktop hover previews show the informational footer beneath the
   *  card art (name, legal activated abilities, and keyboard hints). */
  showCardPreviewFooter: boolean;
  /** Screen edge the stack panel docks to and collapses toward. */
  stackDockSide: StackDockSide;
  /** Density of the multi-opponent HUD rail (comfortable two-row vs compact thin row). */
  opponentHudDensity: OpponentHudDensity;
  /** Multiplayer board presentation: one focused opponent, or all opponent seats. */
  multiplayerBoardLayout: MultiplayerBoardLayout;
  aiSeats: AiSeatPref[];
  /** Table-wide cEDH toggle. When true, every AI opponent plays at cEDH
   *  (bracket 5) regardless of its per-seat difficulty, and the AI/human deck
   *  pools are restricted to bracket-5 decks. cEDH is a table property, not a
   *  per-seat difficulty — see `effectiveAiDifficulty` in `services/cedhLock`. */
  cedhMode: boolean;
  aiArchetypeFilter: AiArchetypeFilter;
  aiCoverageFloor: number;
  aiBracketFilter: CommanderBracket[];
  lastFormat: GameFormat | null;
  lastMatchType: MatchType;
  lastPlayerCount: number;
  dismissedFlowHelpNudge: boolean;
  dismissedSandboxToolsNudge: boolean;
  dismissedReportCardNudge: boolean;
  artChain: ArtChainEntry[];
  artOverrides: Record<string, CardArtOverride>;
  /** Persisted board layout (grid bands + per-widget offsets + active preset).
   *  See {@link FlexLayoutConfig}. Edited only in Flex Layout mode. */
  flexLayout: FlexLayoutConfig;
  /** Whether anonymous, identity-free crash & usage telemetry may be sent.
   *  Default on. Gates every send at enqueue time so a mid-session toggle takes
   *  effect immediately. Builds without a `__TELEMETRY_URL__` define never send
   *  regardless. See `services/telemetry.ts`. */
  telemetryEnabled: boolean;
  /** Prefer the shell-managed native engine for eligible desktop AI games. */
  nativeEngineEnabled: boolean;
}

interface PreferencesActions {
  setLanguage: (lng: SupportedLng) => void;
  setCardSize: (size: CardSizePreference) => void;
  setHudLayout: (layout: HudLayout) => void;
  setFollowActiveOpponent: (enabled: boolean) => void;
  setStackDockSide: (side: StackDockSide) => void;
  setOpponentHudDensity: (density: OpponentHudDensity) => void;
  setMultiplayerBoardLayout: (layout: MultiplayerBoardLayout) => void;
  setLogDefaultState: (state: LogDefaultState) => void;
  setBoardBackground: (bg: BoardBackground) => void;
  setCustomBackgroundUrl: (url: string) => void;
  setVfxQuality: (quality: VfxQuality) => void;
  setAnimationSpeedMultiplier: (multiplier: number) => void;
  setPacingMultiplier: (category: PacingCategory, multiplier: number) => void;
  /** Reset every pacing slider (animation speed + per-category) back to 1.0×. */
  resetPacing: () => void;
  /** Reset the entire preferences store to factory defaults. Clears AI seats,
   *  audio levels, board background — everything except persisted multiplayer
   *  reconnect state, which is owned by `multiplayerStore`. */
  resetAllPreferences: () => void;
  setPhaseStops: (stops: PhaseStop[]) => void;
  setPriorityPassingMode: (mode: PriorityPassingMode) => void;
  setMasterVolume: (vol: number) => void;
  setSfxVolume: (vol: number) => void;
  setMusicVolume: (vol: number) => void;
  setSfxMuted: (muted: boolean) => void;
  setMusicMuted: (muted: boolean) => void;
  setMasterMuted: (muted: boolean) => void;
  setAudioThemeId: (id: string) => void;
  addCustomThemeUrl: (id: string, url: string) => void;
  removeCustomThemeUrl: (id: string) => void;
  setBattlefieldCardDisplay: (display: BattlefieldCardDisplay) => void;
  toggleFolderCollapsed: (id: string) => void;
  setCollapsedFolderIds: (ids: string[]) => void;
  setLastSeenChangelogId: (id: number) => void;
  setCommandZoneDisplay: (display: CommandZoneDisplay) => void;
  setCollapseLands: (mode: ZoneCollapseMode) => void;
  setCollapseSupport: (mode: ZoneCollapseMode) => void;
  setTapRotation: (rotation: TapRotation) => void;
  setSpellPaymentMode: (mode: SpellPaymentMode) => void;
  setHandSort: (sort: SortKey) => void;
  setShowKeywordStrip: (show: boolean) => void;
  setBattlefieldPeekOnHover: (enabled: boolean) => void;
  setCardPreviewMode: (mode: CardPreviewMode) => void;
  setCardPreviewHoverDelayMs: (ms: number) => void;
  setShowCardPreviewFooter: (show: boolean) => void;
  setAiSeatDifficulty: (index: number, difficulty: AIDifficulty) => void;
  setAiSeatDeckId: (index: number, id: AiDeckSelection) => void;
  /** Grow or shrink `aiSeats` to `count` slots. New slots inherit defaults;
   *  shrinking truncates trailing slots. Called whenever the player count
   *  changes so the UI always has exactly `playerCount - 1` panels to render. */
  ensureAiSeatCount: (count: number) => void;
  /** Toggle the table-wide cEDH mode (all AI play cEDH, deck pools → bracket 5). */
  setCedhMode: (enabled: boolean) => void;
  setAiArchetypeFilter: (filter: AiArchetypeFilter) => void;
  setAiCoverageFloor: (floor: number) => void;
  setAiBracketFilter: (brackets: CommanderBracket[]) => void;
  setLastFormat: (format: GameFormat) => void;
  setLastMatchType: (matchType: MatchType) => void;
  setLastPlayerCount: (count: number) => void;
  setDismissedFlowHelpNudge: (dismissed: boolean) => void;
  setDismissedSandboxToolsNudge: (dismissed: boolean) => void;
  setDismissedReportCardNudge: (dismissed: boolean) => void;
  addArtChainEntry: (entry: ArtChainEntry) => void;
  removeArtChainEntry: (index: number) => void;
  moveArtChainEntry: (fromIndex: number, toIndex: number) => void;
  setArtChain: (chain: ArtChainEntry[]) => void;
  setArtOverride: (oracleId: string, override: CardArtOverride) => void;
  clearArtOverride: (oracleId: string) => void;
  clearAllArtOverrides: () => void;
  /** Resize one board grid band (top or bottom); the `1fr` middle absorbs the
   *  change. Flips `activePreset` to "custom". */
  setFlexBand: (side: "top" | "bottom", track: CappedTrack) => void;
  /** Reposition a shared-global widget. Flips `activePreset` to "custom". */
  setFlexWidgetOffset: (key: FlexWidgetKey, offset: WidgetOffset) => void;
  /** Reposition the opponent HUD for the current table size only (so 1v1 and
   *  multiplayer keep distinct spots). Flips `activePreset` to "custom". */
  setFlexOpponentHudOffset: (tableSize: FlexTableSize, offset: WidgetOffset) => void;
  /** Set the lands↔support split (lands' share, clamped 0..1). The support
   *  column takes the remainder. Flips `activePreset` to "custom". */
  setFlexLandSupportRatio: (ratio: number) => void;
  /** Set the left-to-right order of the middle-row cells (drag-to-reorder).
   *  Flips `activePreset` to "custom". */
  setFlexMiddleRowOrder: (order: MiddleCell[]) => void;
  /** Set a zone's aspect-preserving size multiplier. Flips `activePreset` to
   *  "custom". */
  setFlexScale: (key: FlexScaleKey, scale: number) => void;
  /** Set a middle-row cell's content alignment. Flips `activePreset` to
   *  "custom". */
  setFlexCellAlign: (cell: MiddleCell, align: CellAlign) => void;
  /** Apply a preset wholesale — replaces every field, including the opponent
   *  HUD, and sets `activePreset` to the preset's id. Caller resolves the id to
   *  a config (from `presets.ts`) to keep the store free of a preset import. */
  applyFlexPreset: (config: FlexLayoutConfig) => void;
  /** Reset the layout to the default preset (clears all offsets). */
  resetFlexLayout: () => void;
  /** Toggle anonymous crash & usage telemetry. */
  setTelemetryEnabled: (enabled: boolean) => void;
  setNativeEngineEnabled: (enabled: boolean) => void;
}

type LegacyFlatAiPrefs = Partial<{
  aiDifficulty: AIDifficulty;
  aiDeckName: AiDeckSelection;
}>;

type LegacyAiSeatPref = Partial<{
  difficulty: AIDifficulty;
  deckName: AiDeckSelection;
  deckId: AiDeckSelection;
}>;

let strategyCacheClearFn: (() => void) | null = null;

export function registerStrategyCacheClearFn(fn: () => void): void {
  strategyCacheClearFn = fn;
}

function legacyAiDeckNameToId(name: string): string {
  return `saved:${name}`;
}

function migrateAiSeat(seat: LegacyAiSeatPref): AiSeatPref {
  const deckId = seat.deckId ?? (
    seat.deckName && seat.deckName !== AI_DECK_RANDOM
      ? legacyAiDeckNameToId(seat.deckName)
      : AI_DECK_RANDOM
  );
  return {
    difficulty: seat.difficulty ?? DEFAULT_AI_DIFFICULTY,
    deckId,
  };
}

export const usePreferencesStore = create<PreferencesState & PreferencesActions>()(
  persist(
    (set) => ({
      ...buildDefaultPreferences(),

      // Store owns the language; i18n/index.ts subscribes and mirrors it into i18next.
      setLanguage: (lng) => set({ language: lng }),
      setCardSize: (size) => set({ cardSize: size }),
      setHudLayout: (layout) => set({ hudLayout: layout }),
      setFollowActiveOpponent: (enabled) => set({ followActiveOpponent: enabled }),
      setStackDockSide: (side) => set({ stackDockSide: side }),
      setOpponentHudDensity: (density) => set({ opponentHudDensity: density }),
      setMultiplayerBoardLayout: (layout) => set({ multiplayerBoardLayout: layout }),
      setLogDefaultState: (state) => set({ logDefaultState: state }),
      setBoardBackground: (bg) => set({ boardBackground: bg }),
      setCustomBackgroundUrl: (url) => set({ customBackgroundUrl: url.trim() }),
      setVfxQuality: (quality) => set({ vfxQuality: quality }),
      setAnimationSpeedMultiplier: (multiplier) =>
        set({ animationSpeedMultiplier: clamp(multiplier, ANIMATION_SPEED_MIN, ANIMATION_SPEED_MAX) }),
      setPacingMultiplier: (category, multiplier) =>
        set((state) => ({
          pacingMultipliers: {
            ...state.pacingMultipliers,
            [category]: clamp(multiplier, PACING_MIN, PACING_MAX),
          },
        })),
      resetPacing: () =>
        set({
          animationSpeedMultiplier: ANIMATION_SPEED_DEFAULT,
          pacingMultipliers: defaultPacingMultipliers(),
        }),
      resetAllPreferences: () => set(buildDefaultPreferences()),
      setPhaseStops: (stops) => set({ phaseStops: stops }),
      setPriorityPassingMode: (mode) => set({ priorityPassingMode: mode }),
      setMasterVolume: (vol) => set({ masterVolume: vol }),
      setSfxVolume: (vol) => set({ sfxVolume: vol }),
      setMusicVolume: (vol) => set({ musicVolume: vol }),
      setSfxMuted: (muted) => set({ sfxMuted: muted }),
      setMusicMuted: (muted) => set({ musicMuted: muted }),
      setMasterMuted: (muted) => set({ masterMuted: muted }),
      setAudioThemeId: (id) => set({ audioThemeId: id }),
      addCustomThemeUrl: (id, url) =>
        set((state) => ({
          customThemeUrls: [...state.customThemeUrls, { id, url }],
        })),
      removeCustomThemeUrl: (id) =>
        set((state) => ({
          customThemeUrls: state.customThemeUrls.filter((e) => e.id !== id),
          ...(state.audioThemeId === id ? { audioThemeId: "planeswalker" } : {}),
        })),
      setBattlefieldCardDisplay: (display) => set({ battlefieldCardDisplay: display }),
      toggleFolderCollapsed: (id) =>
        set((state) => ({
          collapsedFolderIds: state.collapsedFolderIds.includes(id)
            ? state.collapsedFolderIds.filter((existing) => existing !== id)
            : [...state.collapsedFolderIds, id],
        })),
      setCollapsedFolderIds: (ids) => set({ collapsedFolderIds: ids }),
      setLastSeenChangelogId: (id) => set({ lastSeenChangelogId: id }),
      setCommandZoneDisplay: (display) => set({ commandZoneDisplay: display }),
      setCollapseLands: (mode) => set({ collapseLands: mode }),
      setCollapseSupport: (mode) => set({ collapseSupport: mode }),
      setTapRotation: (rotation) => set({ tapRotation: rotation }),
      setSpellPaymentMode: (mode) => set({ spellPaymentMode: mode }),
      setHandSort: (sort) => set({ handSort: sort }),
      setShowKeywordStrip: (show) => set({ showKeywordStrip: show }),
      setBattlefieldPeekOnHover: (enabled) => set({ battlefieldPeekOnHover: enabled }),
      setCardPreviewMode: (mode) => set({ cardPreviewMode: mode }),
      setCardPreviewHoverDelayMs: (ms) =>
        set({
          cardPreviewHoverDelayMs: clamp(
            ms,
            CARD_PREVIEW_HOVER_DELAY_MIN,
            CARD_PREVIEW_HOVER_DELAY_MAX,
          ),
        }),
      setShowCardPreviewFooter: (show) => set({ showCardPreviewFooter: show }),
      setAiSeatDifficulty: (index, difficulty) =>
        set((state) => {
          if (index < 0 || index >= state.aiSeats.length) return state;
          const next = state.aiSeats.slice();
          next[index] = { ...next[index], difficulty };
          return { aiSeats: next };
        }),
      setAiSeatDeckId: (index, deckId) =>
        set((state) => {
          if (index < 0 || index >= state.aiSeats.length) return state;
          const next = state.aiSeats.slice();
          next[index] = { ...next[index], deckId };
          return { aiSeats: next };
        }),
      ensureAiSeatCount: (count) =>
        set((state) => {
          const target = Math.max(1, count);
          if (state.aiSeats.length === target) return state;
          if (state.aiSeats.length > target) {
            return { aiSeats: state.aiSeats.slice(0, target) };
          }
          const template = state.aiSeats[0] ?? defaultAiSeat();
          const grown = state.aiSeats.slice();
          while (grown.length < target) {
            grown.push({ ...template });
          }
          return { aiSeats: grown };
        }),
      setCedhMode: (enabled) => set({ cedhMode: enabled }),
      setAiArchetypeFilter: (filter) => set({ aiArchetypeFilter: filter }),
      setAiCoverageFloor: (floor) => set({ aiCoverageFloor: floor }),
      setAiBracketFilter: (brackets) => set({ aiBracketFilter: brackets }),
      setLastFormat: (format) => set({ lastFormat: format }),
      setLastMatchType: (matchType) => set({ lastMatchType: matchType }),
      setLastPlayerCount: (count) => set({ lastPlayerCount: count }),
      setDismissedFlowHelpNudge: (dismissed) => set({ dismissedFlowHelpNudge: dismissed }),
      setDismissedSandboxToolsNudge: (dismissed) => set({ dismissedSandboxToolsNudge: dismissed }),
      setDismissedReportCardNudge: (dismissed) => set({ dismissedReportCardNudge: dismissed }),
      addArtChainEntry: (entry) =>
        set((state) => {
          const isDuplicate = state.artChain.some((e) =>
            e.type === entry.type && (e.type !== "set" || (entry.type === "set" && e.setCode === entry.setCode)),
          );
          if (isDuplicate) return state;
          strategyCacheClearFn?.();
          return { artChain: [...state.artChain, entry] };
        }),
      removeArtChainEntry: (index) =>
        set((state) => {
          if (index < 0 || index >= state.artChain.length) return state;
          strategyCacheClearFn?.();
          return { artChain: state.artChain.filter((_, i) => i !== index) };
        }),
      moveArtChainEntry: (fromIndex, toIndex) =>
        set((state) => {
          if (
            fromIndex < 0 || fromIndex >= state.artChain.length ||
            toIndex < 0 || toIndex >= state.artChain.length ||
            fromIndex === toIndex
          ) return state;
          const next = state.artChain.slice();
          const [moved] = next.splice(fromIndex, 1);
          next.splice(toIndex, 0, moved);
          strategyCacheClearFn?.();
          return { artChain: next };
        }),
      setArtChain: (chain) => {
        strategyCacheClearFn?.();
        set({ artChain: chain });
      },
      setArtOverride: (oracleId, override) =>
        set((state) => ({
          artOverrides: { ...state.artOverrides, [oracleId]: override },
        })),
      clearArtOverride: (oracleId) =>
        set((state) => {
          const { [oracleId]: _, ...rest } = state.artOverrides;
          void _;
          return { artOverrides: rest };
        }),
      clearAllArtOverrides: () => set({ artOverrides: {} }),
      setFlexBand: (side, track) =>
        set((state) => ({
          flexLayout: {
            ...state.flexLayout,
            gridBands: { ...state.flexLayout.gridBands, [side]: track },
            activePreset: "custom",
          },
        })),
      setFlexWidgetOffset: (key, offset) =>
        set((state) => ({
          flexLayout: {
            ...state.flexLayout,
            widgets: { ...state.flexLayout.widgets, [key]: offset },
            activePreset: "custom",
          },
        })),
      setFlexOpponentHudOffset: (tableSize, offset) =>
        set((state) => ({
          flexLayout: {
            ...state.flexLayout,
            opponentHudByTableSize: {
              ...state.flexLayout.opponentHudByTableSize,
              [tableSize]: offset,
            },
            activePreset: "custom",
          },
        })),
      setFlexLandSupportRatio: (ratio) =>
        set((state) => ({
          flexLayout: {
            ...state.flexLayout,
            // Clamp so neither column starves (each keeps ≥20% of the row).
            landSupportRatio: Math.min(0.8, Math.max(0.2, ratio)),
            activePreset: "custom",
          },
        })),
      setFlexMiddleRowOrder: (order) =>
        set((state) => ({
          flexLayout: {
            ...state.flexLayout,
            middleRowOrder: order,
            activePreset: "custom",
          },
        })),
      setFlexScale: (key, scale) =>
        set((state) => ({
          flexLayout: {
            ...state.flexLayout,
            // Clamp to a sane, readable range (half to double the auto-size).
            scales: { ...state.flexLayout.scales, [key]: Math.min(2, Math.max(0.5, scale)) },
            activePreset: "custom",
          },
        })),
      setFlexCellAlign: (cell, align) =>
        set((state) => ({
          flexLayout: {
            ...state.flexLayout,
            cellAlign: { ...state.flexLayout.cellAlign, [cell]: align },
            activePreset: "custom",
          },
        })),
      applyFlexPreset: (config) => set({ flexLayout: cloneFlexLayout(config) }),
      resetFlexLayout: () => set({ flexLayout: defaultFlexLayout() }),
      setTelemetryEnabled: (enabled) => set({ telemetryEnabled: enabled }),
      setNativeEngineEnabled: (enabled) => set({ nativeEngineEnabled: enabled }),
    }),
    {
      name: "phase-preferences",
      version: 28,
      // v0 → v1: flat aiDifficulty + aiDeckName become aiSeats[0].
      // v1 → v2: discrete animationSpeed/combatPacing enums become numeric
      //          animationSpeedMultiplier/combatPacingMultiplier.
      // v2 → v3: combatPacingMultiplier folded into a per-category
      //          pacingMultipliers map. The old combat value seeds
      //          pacingMultipliers.combat so existing users keep their
      //          combat slider exactly where they left it; other
      //          categories start at the neutral 1.0 default.
      // v3 → v4: AI deck selections become stable catalog IDs instead
      //          of display names.
      // v4 → v5: Add artStrategy and artOverrides for card art preferences.
      // v5 → v6: Replace artStrategy (single enum) with artChain (ordered preference list).
      // v6 → v7: Add aiBracketFilter; legacy stores default to empty (filter off).
      // v7 → v8: Add spellPaymentMode; legacy stores keep default auto.
      // v8 → v9: Add language; legacy/invalid values fall back to the
      //          browser-detected default so existing users keep their locale.
      // v9 → v10: Add stackDockSide; legacy stores default to right (the prior
      //          fixed behavior).
      // v12 → v13: Add cardPreviewMode; legacy stores default to "follow" (the
      //          prior fixed cursor-following behavior) via the shallow merge.
      // v13 → v14: Add cardPreviewHoverDelayMs; legacy stores default to 0
      //          (instant — the prior behavior) via the shallow merge.
      // v14 → v15: Add commandZoneDisplay; legacy stores default to "auto"
      //          via the shallow merge.
      // v15 → v16: Add flexLayout; legacy stores default to defaultFlexLayout()
      //          via the shallow merge. The default bands reproduce today's
      //          gridTemplateRows exactly, so this is a zero-regression seed —
      //          no explicit migration block needed.
      // v16 → v17: Add collapsedFolderIds; legacy stores default to [] (nothing
      //          collapsed — the prior behavior) via the shallow merge.
      // v17 → v18: Add lastSeenChangelogId; legacy stores default to undefined
      //          via the shallow merge. The changelog hook then silently seeds
      //          it to the current latest on first load, so existing users get
      //          no unread dot for entries that predate this upgrade.
      // v18 → v19: Add handSort; legacy stores default to "none" (insertion
      //          order — the prior hand behavior) via the shallow merge.
      // v19 → v20: Add collapseLands/collapseSupport; legacy stores default to
      //          "auto" (the prior threshold-driven collapse) via the shallow
      //          merge, so existing users see no behavior change.
      // v20 → v21: Add multiplayerBoardLayout; legacy stores default to
      //          "focused", preserving the current focused-opponent layout.
      // v21 → v22: Add telemetryEnabled; legacy stores default to `true` (opt-out,
      //          identity-free crash & usage telemetry) via the shallow merge —
      //          no explicit migration block needed (see flexLayout precedent).
      // v22 → v23: phaseStops gained a turn-direction scope; each legacy bare
      //          Phase string maps to a scoped stop defaulting to "AllTurns"
      //          (fire on every turn = the old behavior).
      // v23 → v24: Add dismissedReportCardNudge; legacy stores default to `false`
      //          (nudge not yet dismissed) via the shallow merge — no explicit
      //          migration block needed (see telemetryEnabled precedent).
      // v24 → v25: Add priorityPassingMode. Standard preserves the prior
      //          recommendation behavior; invalid persisted values normalize
      //          to Standard rather than enabling the experimental mode.
      // v25 → v26: Rename the experimental Smart value to the behavior it
      //          actually enables: SkipLowUseWindows.
      // v26 → v27: Add nativeEngineEnabled; the default-state shallow merge
      //             preserves the intended enabled-by-default behavior.
      // v27 → v28: Add showCardPreviewFooter; legacy stores default to true
      //          via the shallow merge, preserving the prior presentation.
      migrate: (persisted: unknown, version: number) => {
        if (!persisted || typeof persisted !== "object") return persisted;
        let migrated = persisted as Record<string, unknown>;

        if (version < 1) {
          const legacy = migrated as LegacyFlatAiPrefs & Record<string, unknown>;
          const seat: AiSeatPref = {
            difficulty: legacy.aiDifficulty ?? DEFAULT_AI_DIFFICULTY,
            deckId: legacy.aiDeckName === AI_DECK_RANDOM || !legacy.aiDeckName
              ? AI_DECK_RANDOM
              : legacyAiDeckNameToId(legacy.aiDeckName),
          };
          const { aiDifficulty: _d, aiDeckName: _n, ...rest } = legacy;
          void _d;
          void _n;
          migrated = { ...rest, aiSeats: [seat] };
        }

        if (version < 2) {
          const { animationSpeed, combatPacing, ...rest } = migrated as {
            animationSpeed?: string;
            combatPacing?: string;
          } & Record<string, unknown>;
          const legacyAnim =
            typeof animationSpeed === "string"
              ? LEGACY_ANIMATION_SPEED_MULTIPLIERS[animationSpeed]
              : undefined;
          const legacyCombat =
            typeof combatPacing === "string"
              ? LEGACY_COMBAT_PACING_MULTIPLIERS[combatPacing]
              : undefined;
          migrated = {
            ...rest,
            animationSpeedMultiplier: legacyAnim ?? ANIMATION_SPEED_DEFAULT,
            combatPacingMultiplier: legacyCombat ?? PACING_DEFAULT,
          };
        }

        if (version < 3) {
          const { combatPacingMultiplier, ...rest } = migrated as {
            combatPacingMultiplier?: number;
          } & Record<string, unknown>;
          const carried =
            typeof combatPacingMultiplier === "number" && !Number.isNaN(combatPacingMultiplier)
              ? combatPacingMultiplier
              : PACING_DEFAULT;
          migrated = {
            ...rest,
            pacingMultipliers: { ...defaultPacingMultipliers(), combat: carried },
          };
        }

        if (version < 4) {
          const legacy = migrated as { aiSeats?: LegacyAiSeatPref[] } & Record<string, unknown>;
          migrated = {
            ...legacy,
            aiSeats: Array.isArray(legacy.aiSeats) && legacy.aiSeats.length > 0
              ? legacy.aiSeats.map(migrateAiSeat)
              : [defaultAiSeat()],
          };
        }

        if (version < 5) {
          migrated = { ...migrated, artStrategy: "default", artOverrides: {} };
        }

        if (version < 6) {
          const oldStrategy = migrated.artStrategy as string | undefined;
          const STRATEGY_TO_CHAIN: Record<string, ArtChainEntry[]> = {
            newest: [{ type: "newest" }],
            oldest: [{ type: "oldest" }],
            prefer_borderless: [{ type: "prefer_borderless" }],
            prefer_extended: [{ type: "prefer_extended" }],
          };
          const artChain = (oldStrategy && STRATEGY_TO_CHAIN[oldStrategy]) ?? [];
          const { artStrategy: _oldStrat, ...rest } = migrated;
          void _oldStrat;
          migrated = { ...rest, artChain };
        }

        // v6 → v7: introduce aiBracketFilter; existing users default to "off" ([]).
        if (version < 7) {
          const legacy = migrated as { aiBracketFilter?: unknown } & Record<string, unknown>;
          migrated = {
            ...legacy,
            aiBracketFilter: Array.isArray(legacy.aiBracketFilter) ? legacy.aiBracketFilter : [],
          };
        }

        if (version < 8) {
          migrated = { ...migrated, spellPaymentMode: "auto" };
        }

        if (version < 9) {
          const lng = (migrated as { language?: unknown }).language;
          migrated = {
            ...migrated,
            language:
              typeof lng === "string" && (SUPPORTED_LNGS as readonly string[]).includes(lng)
                ? lng
                : detectInitialLanguage(),
          };
        }

        if (version < 10) {
          migrated = { ...migrated, stackDockSide: "right" };
        }

        // v10 → v11: Add opponentHudDensity; legacy stores default to the
        // comfortable two-row rail (the prior fixed behavior).
        if (version < 11) {
          migrated = { ...migrated, opponentHudDensity: "comfortable" };
        }

        // v11 → v12: cEDH is no longer a per-seat difficulty — it's the
        // table-wide `cedhMode` toggle. Derive the flag from any seat that was
        // set to "CEDH" (the old cascade forced every seat to it) and reset
        // those seats to the default difficulty so the per-seat dropdowns no
        // longer surface "CEDH".
        if (version < 12) {
          const legacy = migrated as {
            aiSeats?: Array<{ difficulty?: string } & Record<string, unknown>>;
          } & Record<string, unknown>;
          const seats = Array.isArray(legacy.aiSeats) ? legacy.aiSeats : [];
          const wasCedh = seats.some((s) => s?.difficulty === "CEDH");
          migrated = {
            ...legacy,
            cedhMode: wasCedh,
            aiSeats: seats.map((s) =>
              s?.difficulty === "CEDH"
                ? { ...s, difficulty: DEFAULT_AI_DIFFICULTY }
                : s,
            ),
          };
        }

        if (version < 21) {
          migrated = { ...migrated, multiplayerBoardLayout: "focused" };
        }

        // v22 → v23: phase stops gained a turn-direction scope. Map each legacy
        // bare Phase string to a scoped stop defaulting to "AllTurns" (fire on
        // every turn = the old behavior). Non-array values reset to [].
        if (version < 23) {
          const legacy = (migrated as { phaseStops?: unknown }).phaseStops;
          migrated = {
            ...migrated,
            phaseStops: Array.isArray(legacy)
              ? legacy.map((p) => (typeof p === "string" ? { phase: p, scope: "AllTurns" } : p))
              : [],
          };
        }

        // v25 → v26: rename the experimental mode and normalize unknown values.
        // This single block also handles older stores that never had the field.
        if (version < 26) {
          const legacy = (migrated as { priorityPassingMode?: unknown }).priorityPassingMode;
          migrated = {
            ...migrated,
            priorityPassingMode:
              legacy === "Smart" || legacy === "SkipLowUseWindows"
                ? "SkipLowUseWindows"
                : "Standard",
          };
        }

        return migrated;
      },
    },
  ),
);
