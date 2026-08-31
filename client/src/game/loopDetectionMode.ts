import type { LoopDetectionMode } from "../adapter/types";

/**
 * The local-game URL is a user-controlled serialization boundary. Keep the
 * query representation beside its parser so every LoopDetectionMode remains
 * selectable and round-trips through GameSetupPage -> GamePage.
 */
export function loopDetectionModeFromQuery(value: string | null): LoopDetectionMode {
  switch (value?.toLowerCase()) {
    // The selector retired the standalone "On" choice in favor of
    // "Interactive" (its surviving semantics); a bookmarked/shared
    // `?loopDetection=on` link must still enable the detector, so it maps
    // forward rather than silently falling through to "Off" below.
    case "on":
      return { type: "Interactive" };
    case "interactive":
      return { type: "Interactive" };
    default:
      return { type: "Off" };
  }
}

export function loopDetectionModeToQuery(mode: LoopDetectionMode): string | null {
  switch (mode.type) {
    case "Off":
      return null;
    case "On":
      return "on";
    case "Interactive":
      return "interactive";
  }

  const unreachable: never = mode;
  return unreachable;
}
