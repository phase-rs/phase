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
  const matchesScope = (entry: GameLogEntry) => {
    if (opts.turn != null && entry.turn !== opts.turn) return false;
    if (opts.categories && opts.categories.size > 0 && !opts.categories.has(entry.category)) {
      return false;
    }
    return true;
  };
  if (!q) return entries.filter(matchesScope);

  const matchesQuery = (entry: GameLogEntry) => {
    const haystack = `${entry.category} ${segmentsToPlainText(entry.segments)}`.toLowerCase();
    return haystack.includes(q);
  };
  const filtered: GameLogEntry[] = [];
  let pendingBoundaries: GameLogEntry[] = [];

  for (const entry of entries) {
    if (!matchesScope(entry)) continue;
    if (entry.presentation?.boundary === "Turn" || entry.presentation?.boundary === "Phase") {
      pendingBoundaries.push(entry);
      continue;
    }
    if (!matchesQuery(entry)) continue;
    filtered.push(...pendingBoundaries, entry);
    pendingBoundaries = [];
  }
  return filtered;
}

export function uniqueTurns(entries: GameLogEntry[]): number[] {
  const turns = new Set<number>();
  for (const entry of entries) {
    if (entry.turn > 0) turns.add(entry.turn);
  }
  return [...turns].sort((a, b) => a - b);
}

export function exportLogEntriesJson(entries: GameLogEntry[]): string {
  return JSON.stringify(entries, null, 2);
}
