import type { GameLogEntry, LogCategory, LogSegment } from "../adapter/types";

export function segmentsToPlainText(segments: LogSegment[]): string {
  return segments
    .map((segment) => {
      switch (segment.type) {
        case "Text":
          return segment.value;
        case "CardName":
          return segment.value.name;
        case "PlayerName":
          return segment.value.name;
        case "Number":
          return String(segment.value);
        case "Mana":
          return segment.value;
        case "Zone":
          return segment.value;
        case "Keyword":
          return segment.value;
      }
    })
    .join("");
}

export function filterLogEntries(
  entries: GameLogEntry[],
  opts: {
    query: string;
    categories: Set<LogCategory> | null;
    turn: number | null;
  },
): GameLogEntry[] {
  const q = opts.query.trim().toLowerCase();
  return entries.filter((entry) => {
    if (opts.turn != null && entry.turn !== opts.turn) return false;
    if (opts.categories && opts.categories.size > 0 && !opts.categories.has(entry.category)) {
      return false;
    }
    if (!q) return true;
    const haystack = `${entry.category} ${segmentsToPlainText(entry.segments)}`.toLowerCase();
    return haystack.includes(q);
  });
}

export function uniqueTurns(entries: GameLogEntry[]): number[] {
  const turns = new Set<number>();
  for (const entry of entries) {
    turns.add(entry.turn);
  }
  return [...turns].sort((a, b) => a - b);
}

export function exportLogEntriesJson(entries: GameLogEntry[]): string {
  return JSON.stringify(entries, null, 2);
}
