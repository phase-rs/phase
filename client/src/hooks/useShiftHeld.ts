import { useEffect } from "react";

import { usePreferencesStore } from "../stores/preferencesStore";
import { useUiStore } from "../stores/uiStore";

/**
 * Tracks whether the Shift key is currently held in `uiStore.shiftHeld`, used by
 * the "shift" card-preview mode (preview shows only while Shift is down,
 * Tabletop-Simulator style).
 *
 * Unlike `useAltToggle` (press-to-toggle, a macOS Option-key workaround), Shift
 * fires reliable keydown/keyup pairs, so this tracks true held-state. A `blur`
 * resets the flag so it can't stick "down" if focus is lost mid-hold (e.g. an
 * alt-tab while Shift is held).
 *
 * Listeners are only attached when the preference is set to "shift", so players
 * on the default mode pay no per-keystroke store-update cost.
 */
export function useShiftHeld(): void {
  const enabled = usePreferencesStore((s) => s.cardPreviewMode === "shift");

  useEffect(() => {
    if (!enabled) {
      // Mode changed away from "shift" — clear any stale held flag.
      useUiStore.getState().setShiftHeld(false);
      return undefined;
    }

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Shift") useUiStore.getState().setShiftHeld(true);
    };
    const onKeyUp = (e: KeyboardEvent) => {
      if (e.key === "Shift") useUiStore.getState().setShiftHeld(false);
    };
    const onBlur = () => useUiStore.getState().setShiftHeld(false);

    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("blur", onBlur);
      useUiStore.getState().setShiftHeld(false);
    };
  }, [enabled]);
}
