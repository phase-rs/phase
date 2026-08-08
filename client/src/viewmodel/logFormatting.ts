import type {
  GameLogEntry,
  LogBoundary,
  LogCategory,
  LogImportance,
  LogPresentation,
  LogTone,
} from "../adapter/types";

export type LogView = "timeline" | "details" | "diagnostics";

export interface LogDivider {
  seq: number;
  turn: number;
  phase: GameLogEntry["phase"];
  boundary: Exclude<LogBoundary, "None">;
}

export type LogTimelineRow =
  | { type: "entry"; entry: GameLogEntry }
  | { type: "divider"; divider: LogDivider };

export function timelineRowSeq(row: LogTimelineRow): number {
  return row.type === "entry" ? row.entry.seq : row.divider.seq;
}

const LEGACY_PRESENTATION: LogPresentation = {
  importance: "Detail",
  tone: "Neutral",
  boundary: "None",
  visibility: "Public",
};

/** The only compatibility seam for persisted pre-presentation entries. */
export function logPresentation(entry: GameLogEntry): LogPresentation {
  return entry.presentation ?? LEGACY_PRESENTATION;
}

function matchesView(importance: LogImportance, view: LogView): boolean {
  switch (view) {
    case "timeline":
      return importance === "Essential" || importance === "Context";
    case "details":
      return importance !== "Diagnostic";
    case "diagnostics":
      return true;
  }
}

/** Filters engine-authored presentation metadata; no category/text heuristic is used. */
export function filterLogByView(
  entries: GameLogEntry[],
  view: LogView,
  categories: Set<LogCategory> | null = null,
  showHiddenInformation = false,
): GameLogEntry[] {
  return entries.filter((entry) => {
    if (
      logPresentation(entry).visibility === "HiddenInformation"
      && (view !== "diagnostics" || !showHiddenInformation)
    ) {
      return false;
    }
    const explicitCategory = categories?.has(entry.category) ?? false;
    if (categories && !explicitCategory) return false;

    const presentation = logPresentation(entry);
    if (entry.category === "Debug") return view === "diagnostics" || explicitCategory;
    if (explicitCategory) return true;
    if (!entry.presentation) return view !== "timeline";
    return matchesView(presentation.importance, view);
  });
}

/**
 * Replaces retained boundary entries with a divider before the next retained
 * content row. A pending turn and phase coalesce into one divider. Explicit
 * category drill-downs may retain a standalone nonzero-turn boundary.
 */
export function timelineRows(
  entries: GameLogEntry[],
  retainStandaloneBoundary = false,
): LogTimelineRow[] {
  const rows: LogTimelineRow[] = [];
  let pending: LogDivider | null = null;

  for (const entry of entries) {
    const presentation = logPresentation(entry);
    if (presentation.boundary !== "None") {
      if (retainStandaloneBoundary && pending !== null && pending.turn !== 0) {
        rows.push({ type: "divider", divider: pending });
      }
      pending = {
        seq: entry.seq,
        turn: entry.turn,
        phase: entry.phase,
        boundary: presentation.boundary,
      };
      continue;
    }
    if (pending?.turn === 0) {
      pending = null;
    } else if (pending) {
      rows.push({ type: "divider", divider: pending });
      pending = null;
    }
    rows.push({ type: "entry", entry });
  }
  if (retainStandaloneBoundary && pending !== null && pending.turn !== 0) {
    rows.push({ type: "divider", divider: pending });
  }
  return rows;
}

export function toneClass(tone: LogTone): string {
  switch (tone) {
    case "Positive":
      return "border-l-emerald-400 text-emerald-300";
    case "Negative":
      return "border-l-red-400 text-red-300";
    case "Informational":
      return "border-l-cyan-400 text-cyan-300";
    case "Diagnostic":
      return "border-l-fuchsia-400 text-fuchsia-300";
    case "Neutral":
      return "border-l-gray-600 text-gray-400";
  }
}
