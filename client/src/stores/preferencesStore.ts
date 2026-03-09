import { create } from "zustand";
import { persist } from "zustand/middleware";

import type { AnimationSpeed, VfxQuality } from "../animation/types";

export type CardSizePreference = "small" | "medium" | "large";
export type HudLayout = "inline" | "floating";
export type LogDefaultState = "open" | "closed";
export type BoardBackground =
  | "auto-wubrg"
  | "white"
  | "blue"
  | "black"
  | "red"
  | "green"
  | "none";

interface PreferencesState {
  cardSize: CardSizePreference;
  hudLayout: HudLayout;
  logDefaultState: LogDefaultState;
  boardBackground: BoardBackground;
  vfxQuality: VfxQuality;
  animationSpeed: AnimationSpeed;
}

interface PreferencesActions {
  setCardSize: (size: CardSizePreference) => void;
  setHudLayout: (layout: HudLayout) => void;
  setLogDefaultState: (state: LogDefaultState) => void;
  setBoardBackground: (bg: BoardBackground) => void;
  setVfxQuality: (quality: VfxQuality) => void;
  setAnimationSpeed: (speed: AnimationSpeed) => void;
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

      setCardSize: (size) => set({ cardSize: size }),
      setHudLayout: (layout) => set({ hudLayout: layout }),
      setLogDefaultState: (state) => set({ logDefaultState: state }),
      setBoardBackground: (bg) => set({ boardBackground: bg }),
      setVfxQuality: (quality) => set({ vfxQuality: quality }),
      setAnimationSpeed: (speed) => set({ animationSpeed: speed }),
    }),
    { name: "forge-preferences" },
  ),
);
