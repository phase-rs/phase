import { create } from "zustand";
import { persist } from "zustand/middleware";

import type { GameFormat, MatchType, Phase } from "../adapter/types";
import type { AnimationSpeed, CombatPacing, VfxQuality } from "../animation/types";
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

interface PreferencesState {
  cardSize: CardSizePreference;
  hudLayout: HudLayout;
  followActiveOpponent: boolean;
  logDefaultState: LogDefaultState;
  boardBackground: BoardBackground;
  customBackgroundUrl: string;
  vfxQuality: VfxQuality;
  animationSpeed: AnimationSpeed;
  combatPacing: CombatPacing;
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
}

interface PreferencesActions {
  setCardSize: (size: CardSizePreference) => void;
  setHudLayout: (layout: HudLayout) => void;
  setFollowActiveOpponent: (enabled: boolean) => void;
  setLogDefaultState: (state: LogDefaultState) => void;
  setBoardBackground: (bg: BoardBackground) => void;
  setCustomBackgroundUrl: (url: string) => void;
  setVfxQuality: (quality: VfxQuality) => void;
  setAnimationSpeed: (speed: AnimationSpeed) => void;
  setCombatPacing: (pacing: CombatPacing) => void;
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
}

type LegacyFlatAiPrefs = Partial<{
  aiDifficulty: AIDifficulty;
  aiDeckName: AiDeckSelection;
}>;

export const usePreferencesStore = create<PreferencesState & PreferencesActions>()(
  persist(
    (set) => ({
      cardSize: "medium",
      hudLayout: "inline",
      followActiveOpponent: false,
      logDefaultState: "closed",
      boardBackground: "auto-wubrg",
      customBackgroundUrl: "",
      vfxQuality: "full",
      animationSpeed: "normal",
      combatPacing: "normal",
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

      setCardSize: (size) => set({ cardSize: size }),
      setHudLayout: (layout) => set({ hudLayout: layout }),
      setFollowActiveOpponent: (enabled) => set({ followActiveOpponent: enabled }),
      setLogDefaultState: (state) => set({ logDefaultState: state }),
      setBoardBackground: (bg) => set({ boardBackground: bg }),
      setCustomBackgroundUrl: (url) => set({ customBackgroundUrl: url.trim() }),
      setVfxQuality: (quality) => set({ vfxQuality: quality }),
      setAnimationSpeed: (speed) => set({ animationSpeed: speed }),
      setCombatPacing: (pacing) => set({ combatPacing: pacing }),
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
    }),
    {
      name: "phase-preferences",
      version: 1,
      // v0 → v1: flat aiDifficulty + aiDeckName become aiSeats[0]. Any other
      // legacy fields pass through untouched — persist merges against the
      // current defaults on rehydrate, so unknown v0 fields are simply dropped
      // and missing v1 fields get their default.
      migrate: (persisted: unknown, version: number) => {
        if (!persisted || typeof persisted !== "object") return persisted;
        if (version >= 1) return persisted;
        const legacy = persisted as LegacyFlatAiPrefs & Record<string, unknown>;
        const seat: AiSeatPref = {
          difficulty: legacy.aiDifficulty ?? DEFAULT_AI_DIFFICULTY,
          deckName: legacy.aiDeckName ?? AI_DECK_RANDOM,
        };
        // Strip legacy flat keys so they don't leak into the new schema.
        const { aiDifficulty: _d, aiDeckName: _n, ...rest } = legacy;
        void _d;
        void _n;
        return { ...rest, aiSeats: [seat] };
      },
    },
  ),
);
