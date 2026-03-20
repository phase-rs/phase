import { create } from "zustand";
import { persist } from "zustand/middleware";

import type { Phase } from "../adapter/types";
import type { AnimationSpeed, CombatPacing, VfxQuality } from "../animation/types";
import type { AIDifficulty } from "../constants/ai";
import { DEFAULT_AI_DIFFICULTY } from "../constants/ai";

export type CardSizePreference = "small" | "medium" | "large";
export type HudLayout = "inline" | "floating";
export type LogDefaultState = "open" | "closed";
export type BattlefieldCardDisplay = "art_crop" | "full_card";
export type TapRotation = "mtga" | "classic";
/** "auto-wubrg" picks a random battlefield matching the dominant mana color.
 *  "random" picks a random battlefield each game regardless of color.
 *  "none" disables the background image. Any other string is a battlefield ID. */
export type BoardBackground = "auto-wubrg" | "random" | "none" | (string & {});

interface PreferencesState {
  cardSize: CardSizePreference;
  hudLayout: HudLayout;
  followActiveOpponent: boolean;
  logDefaultState: LogDefaultState;
  boardBackground: BoardBackground;
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
  battlefieldCardDisplay: BattlefieldCardDisplay;
  tapRotation: TapRotation;
  aiDifficulty: AIDifficulty;
}

interface PreferencesActions {
  setCardSize: (size: CardSizePreference) => void;
  setHudLayout: (layout: HudLayout) => void;
  setFollowActiveOpponent: (enabled: boolean) => void;
  setLogDefaultState: (state: LogDefaultState) => void;
  setBoardBackground: (bg: BoardBackground) => void;
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
  setBattlefieldCardDisplay: (display: BattlefieldCardDisplay) => void;
  setTapRotation: (rotation: TapRotation) => void;
  setAiDifficulty: (difficulty: AIDifficulty) => void;
}

export const usePreferencesStore = create<PreferencesState & PreferencesActions>()(
  persist(
    (set) => ({
      cardSize: "medium",
      hudLayout: "inline",
      followActiveOpponent: false,
      logDefaultState: "closed",
      boardBackground: "auto-wubrg",
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
      battlefieldCardDisplay: "art_crop",
      tapRotation: "mtga",
      aiDifficulty: DEFAULT_AI_DIFFICULTY,

      setCardSize: (size) => set({ cardSize: size }),
      setHudLayout: (layout) => set({ hudLayout: layout }),
      setFollowActiveOpponent: (enabled) => set({ followActiveOpponent: enabled }),
      setLogDefaultState: (state) => set({ logDefaultState: state }),
      setBoardBackground: (bg) => set({ boardBackground: bg }),
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
      setBattlefieldCardDisplay: (display) => set({ battlefieldCardDisplay: display }),
      setTapRotation: (rotation) => set({ tapRotation: rotation }),
      setAiDifficulty: (difficulty) => set({ aiDifficulty: difficulty }),
    }),
    { name: "phase-preferences" },
  ),
);
