import { create } from "zustand";
import { persist } from "zustand/middleware";

import type { GameFormat, MatchType, Phase } from "../adapter/types";
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
  deckName: AiDeckSelection;
}

export type CardSizePreference = "small" | "medium" | "large";
export type HudLayout = "inline" | "floating";
export type LogDefaultState = "open" | "closed";
export type BattlefieldCardDisplay = "art_crop" | "full_card";
export type TapRotation = "mtga" | "classic";
/** "auto-wubrg" picks a random battlefield matching the dominant mana color.
 *  "random" picks a random battlefield each game regardless of color.
 *  "none" disables the background image.
 *  "custom" uses the URL stored in `customBackgroundUrl`.
 *  Any other string is a battlefield or plain-color ID. */
export type BoardBackground = "auto-wubrg" | "random" | "none" | "custom" | (string & {});

function defaultAiSeat(): AiSeatPref {
  return { difficulty: DEFAULT_AI_DIFFICULTY, deckName: AI_DECK_RANDOM };
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
    cardSize: "medium",
    hudLayout: "inline",
    followActiveOpponent: false,
    logDefaultState: "closed",
    boardBackground: "auto-wubrg",
    customBackgroundUrl: "",
    vfxQuality: "full",
    animationSpeedMultiplier: ANIMATION_SPEED_DEFAULT,
    pacingMultipliers: defaultPacingMultipliers(),
    phaseStops: [],
    masterVolume: 100,
    sfxVolume: 70,
    musicVolume: 40,
    sfxMuted: false,
    musicMuted: false,
    masterMuted: false,
    audioThemeId: "planeswalker",
    customThemeUrls: [],
    battlefieldCardDisplay: "art_crop",
    tapRotation: "mtga",
    showKeywordStrip: true,
    aiSeats: [defaultAiSeat()],
    aiArchetypeFilter: "Any",
    aiCoverageFloor: DEFAULT_AI_COVERAGE_FLOOR,
    lastFormat: null,
    lastMatchType: "Bo1",
    lastPlayerCount: 2,
    experimentalFeatures: false,
    useGridLayout: false,
  };
}

interface PreferencesState {
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
  phaseStops: Phase[];
  masterVolume: number;
  sfxVolume: number;
  musicVolume: number;
  sfxMuted: boolean;
  musicMuted: boolean;
  masterMuted: boolean;
  audioThemeId: string;
  customThemeUrls: Array<{ id: string; url: string }>;
  battlefieldCardDisplay: BattlefieldCardDisplay;
  tapRotation: TapRotation;
  showKeywordStrip: boolean;
  aiSeats: AiSeatPref[];
  aiArchetypeFilter: AiArchetypeFilter;
  aiCoverageFloor: number;
  lastFormat: GameFormat | null;
  lastMatchType: MatchType;
  lastPlayerCount: number;
  experimentalFeatures: boolean;
  useGridLayout: boolean;
}

interface PreferencesActions {
  setCardSize: (size: CardSizePreference) => void;
  setHudLayout: (layout: HudLayout) => void;
  setFollowActiveOpponent: (enabled: boolean) => void;
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
  setPhaseStops: (stops: Phase[]) => void;
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
  setTapRotation: (rotation: TapRotation) => void;
  setShowKeywordStrip: (show: boolean) => void;
  setAiSeatDifficulty: (index: number, difficulty: AIDifficulty) => void;
  setAiSeatDeckName: (index: number, name: AiDeckSelection) => void;
  /** Grow or shrink `aiSeats` to `count` slots. New slots inherit defaults;
   *  shrinking truncates trailing slots. Called whenever the player count
   *  changes so the UI always has exactly `playerCount - 1` panels to render. */
  ensureAiSeatCount: (count: number) => void;
  setAiArchetypeFilter: (filter: AiArchetypeFilter) => void;
  setAiCoverageFloor: (floor: number) => void;
  setLastFormat: (format: GameFormat) => void;
  setLastMatchType: (matchType: MatchType) => void;
  setLastPlayerCount: (count: number) => void;
  setExperimentalFeatures: (enabled: boolean) => void;
  setUseGridLayout: (enabled: boolean) => void;
}

type LegacyFlatAiPrefs = Partial<{
  aiDifficulty: AIDifficulty;
  aiDeckName: AiDeckSelection;
}>;

export const usePreferencesStore = create<PreferencesState & PreferencesActions>()(
  persist(
    (set) => ({
      ...buildDefaultPreferences(),

      setCardSize: (size) => set({ cardSize: size }),
      setHudLayout: (layout) => set({ hudLayout: layout }),
      setFollowActiveOpponent: (enabled) => set({ followActiveOpponent: enabled }),
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
      setTapRotation: (rotation) => set({ tapRotation: rotation }),
      setShowKeywordStrip: (show) => set({ showKeywordStrip: show }),
      setAiSeatDifficulty: (index, difficulty) =>
        set((state) => {
          if (index < 0 || index >= state.aiSeats.length) return state;
          const next = state.aiSeats.slice();
          next[index] = { ...next[index], difficulty };
          return { aiSeats: next };
        }),
      setAiSeatDeckName: (index, deckName) =>
        set((state) => {
          if (index < 0 || index >= state.aiSeats.length) return state;
          const next = state.aiSeats.slice();
          next[index] = { ...next[index], deckName };
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
      setAiArchetypeFilter: (filter) => set({ aiArchetypeFilter: filter }),
      setAiCoverageFloor: (floor) => set({ aiCoverageFloor: floor }),
      setLastFormat: (format) => set({ lastFormat: format }),
      setLastMatchType: (matchType) => set({ lastMatchType: matchType }),
      setLastPlayerCount: (count) => set({ lastPlayerCount: count }),
      setExperimentalFeatures: (enabled) => set({ experimentalFeatures: enabled }),
      setUseGridLayout: (enabled) => set({ useGridLayout: enabled }),
    }),
    {
      name: "phase-preferences",
      version: 3,
      // v0 → v1: flat aiDifficulty + aiDeckName become aiSeats[0].
      // v1 → v2: discrete animationSpeed/combatPacing enums become numeric
      //          animationSpeedMultiplier/combatPacingMultiplier.
      // v2 → v3: combatPacingMultiplier folded into a per-category
      //          pacingMultipliers map. The old combat value seeds
      //          pacingMultipliers.combat so existing users keep their
      //          combat slider exactly where they left it; other
      //          categories start at the neutral 1.0 default.
      migrate: (persisted: unknown, version: number) => {
        if (!persisted || typeof persisted !== "object") return persisted;
        let migrated = persisted as Record<string, unknown>;

        if (version < 1) {
          const legacy = migrated as LegacyFlatAiPrefs & Record<string, unknown>;
          const seat: AiSeatPref = {
            difficulty: legacy.aiDifficulty ?? DEFAULT_AI_DIFFICULTY,
            deckName: legacy.aiDeckName ?? AI_DECK_RANDOM,
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

        return migrated;
      },
    },
  ),
);
