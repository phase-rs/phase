import { create } from "zustand";
import { persist } from "zustand/middleware";

import type { Phase } from "../adapter/types";
import type { AnimationSpeed, VfxQuality } from "../animation/types";

export type CardSizePreference = "small" | "medium" | "large";
export type HudLayout = "inline" | "floating";
export type LogDefaultState = "open" | "closed";
/** "auto-wubrg" picks a random battlefield matching the dominant mana color.
 *  "none" disables the background image. Any other string is a battlefield ID. */
export type BoardBackground = "auto-wubrg" | "none" | (string & {});

interface PreferencesState {
  cardSize: CardSizePreference;
  hudLayout: HudLayout;
  logDefaultState: LogDefaultState;
  boardBackground: BoardBackground;
  vfxQuality: VfxQuality;
  animationSpeed: AnimationSpeed;
  phaseStops: Phase[];
}

interface PreferencesActions {
  setCardSize: (size: CardSizePreference) => void;
  setHudLayout: (layout: HudLayout) => void;
  setLogDefaultState: (state: LogDefaultState) => void;
  setBoardBackground: (bg: BoardBackground) => void;
  setVfxQuality: (quality: VfxQuality) => void;
  setAnimationSpeed: (speed: AnimationSpeed) => void;
  setPhaseStops: (stops: Phase[]) => void;
}

export const usePreferencesStore = create<PreferencesState & PreferencesActions>()(
  persist(
    (set) => ({
      cardSize: "medium",
      hudLayout: "inline",
      logDefaultState: "closed",
      boardBackground: "auto-wubrg",
      vfxQuality: "full",
      animationSpeed: "normal",
      phaseStops: [],

      setCardSize: (size) => set({ cardSize: size }),
      setHudLayout: (layout) => set({ hudLayout: layout }),
      setLogDefaultState: (state) => set({ logDefaultState: state }),
      setBoardBackground: (bg) => set({ boardBackground: bg }),
      setVfxQuality: (quality) => set({ vfxQuality: quality }),
      setAnimationSpeed: (speed) => set({ animationSpeed: speed }),
      setPhaseStops: (stops) => set({ phaseStops: stops }),
    }),
    { name: "forge-preferences" },
  ),
);
