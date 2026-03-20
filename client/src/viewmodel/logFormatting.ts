import type { GameLogEntry, LogCategory } from "../adapter/types";

export type LogVerbosity = "full" | "compact" | "minimal";

const COMPACT_EXCLUDE: Set<LogCategory> = new Set(["Mana", "State", "Turn"]);

const MINIMAL_INCLUDE: Set<LogCategory> = new Set([
  "Game", "Stack", "Combat", "Life", "Destroy", "Token",
]);

/** ZoneChanged entries contain Zone segments ("moves from X to Y"). Other Zone-category
 *  entries (LandPlayed, CardsDrawn, Discarded, etc.) don't — they're worth keeping. */
function isZoneChangedEntry(entry: GameLogEntry): boolean {
  return entry.category === "Zone" && entry.segments.some((s) => s.type === "Zone");
}

export function filterLogByVerbosity(
  entries: GameLogEntry[],
  level: LogVerbosity,
): GameLogEntry[] {
  switch (level) {
    case "full":
      return entries;
    case "compact":
      return entries.filter((e) => {
        if (COMPACT_EXCLUDE.has(e.category)) {
          // Keep TurnStarted entries (category: Turn, first segment is "Turn ")
          if (e.category === "Turn") {
            return e.segments.length > 0 && e.segments[0].type === "Text" && e.segments[0].value === "Turn ";
          }
          return false;
        }
        // Filter out individual ZoneChanged ("X moves from Y to Z") but keep
        // LandPlayed, CardsDrawn, Discarded, Cycled, CardsRevealed
        if (isZoneChangedEntry(e)) return false;
        return true;
      });
    case "minimal":
      return entries.filter((e) => MINIMAL_INCLUDE.has(e.category));
  }
}

/** Get the CSS color class for a log entry based on its category. */
export function categoryColorClass(entry: GameLogEntry): string {
  switch (entry.category) {
    case "Combat":
    case "Destroy":
      return "text-red-400";
    case "Stack":
      return "text-blue-400";
    case "Life":
      // Detect gain vs loss from segments
      if (entry.segments.some((s) => s.type === "Text" && s.value === " gains ")) {
        return "text-green-400";
      }
      return "text-red-400";
    case "Special":
      return "text-amber-400";
    default:
      return "text-gray-400";
  }
}
