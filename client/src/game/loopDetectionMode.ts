import type { LoopDetectionMode } from "../adapter/types";

/**
 * The local-game URL is a user-controlled serialization boundary. Keep the
 * query representation beside its parser so every LoopDetectionMode remains
 * selectable and round-trips through GameSetupPage -> GamePage.
 */
export function loopDetectionModeFromQuery(value: string | null): LoopDetectionMode {
  switch (value?.toLowerCase()) {
    case "on":
      return { type: "On" };
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
