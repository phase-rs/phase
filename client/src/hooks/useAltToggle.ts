import { useEffect } from "react";

import { useUiStore } from "../stores/uiStore";

/**
 * Toggles uiStore.altHeld on Alt key press.
 * macOS fires instant synthetic keyup for Option key (~0.8ms after keydown),
 * making hold-to-show impossible — press-to-toggle instead.
 *
 * Shared between GamePage and DeckBuilderPage for the parsed-abilities preview.
 */
export function useAltToggle(): void {
  useEffect(() => {
    function onAltToggle(e: KeyboardEvent) {
      if (e.key === "Alt" && !e.repeat) {
        e.preventDefault();
        const store = useUiStore.getState();
        store.setAltHeld(!store.altHeld);
      }
    }
    window.addEventListener("keydown", onAltToggle, true);
    return () => window.removeEventListener("keydown", onAltToggle, true);
  }, []);
}
