import { useSyncExternalStore } from "react";

export const LANDSCAPE_GAME_REQUIREMENT_QUERY =
  "(orientation: portrait) and (hover: none) and (pointer: coarse)";

function portraitMobileSnapshot(): boolean {
  return typeof window !== "undefined"
    && typeof window.matchMedia === "function"
    && window.matchMedia(LANDSCAPE_GAME_REQUIREMENT_QUERY).matches;
}

function subscribeToPortraitMobile(onChange: () => void): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return () => undefined;
  }

  const query = window.matchMedia(LANDSCAPE_GAME_REQUIREMENT_QUERY);
  query.addEventListener("change", onChange);
  return () => query.removeEventListener("change", onChange);
}

/** True when touch-first gameplay must pause behind the landscape gate. */
export function useLandscapeGameRequirement(): boolean {
  return useSyncExternalStore(
    subscribeToPortraitMobile,
    portraitMobileSnapshot,
    () => false,
  );
}
