import { create } from "zustand";

import type { ManaColor, MatchMode, MatchOutcome, MatchRecord } from "../services/matchHistoryPersistence";
import {
  clearMatchHistory,
  deleteMatchRecord,
  loadMatchHistory,
  saveMatchRecord,
} from "../services/matchHistoryPersistence";

// ── Derived statistics types ──────────────────────────────────────────────────

export interface WinLossRecord {
  wins: number;
  losses: number;
  draws: number;
  total: number;
  winRate: number;
}

export interface FormatStats {
  format: string;
  record: WinLossRecord;
}

export interface DeckStats {
  deckName: string;
  colors: ManaColor[];
  record: WinLossRecord;
  avgTurns: number;
  avgDuration: number;
  lastPlayedAt: number;
}

export interface ColorStats {
  color: ManaColor;
  record: WinLossRecord;
}

export interface HistoryStats {
  overall: WinLossRecord;
  byFormat: FormatStats[];
  byDeck: DeckStats[];
  byColor: ColorStats[];
  avgTurnCount: number;
  avgDurationSec: number;
  longestWinStreak: number;
  currentStreak: { type: MatchOutcome | null; count: number };
  mostPlayedFormat: string | null;
  mostPlayedDeck: string | null;
}

// ── Filter / sort types ───────────────────────────────────────────────────────

export type HistorySortKey = "date" | "turns" | "duration";
export type HistorySortDir = "asc" | "desc";

export interface HistoryFilters {
  outcome: MatchOutcome | "all";
  format: string | "all";
  mode: MatchMode | "all";
  deckName: string | "all";
  dateFrom: number | null;
  dateTo: number | null;
}

// ── Store state ───────────────────────────────────────────────────────────────

interface MatchHistoryState {
  records: MatchRecord[];
  loaded: boolean;
  filters: HistoryFilters;
  sortKey: HistorySortKey;
  sortDir: HistorySortDir;

  // Actions
  loadAll: () => Promise<void>;
  addRecord: (record: MatchRecord) => Promise<void>;
  removeRecord: (id: string) => Promise<void>;
  clearAll: () => Promise<void>;
  setFilters: (patch: Partial<HistoryFilters>) => void;
  resetFilters: () => void;
  setSortKey: (key: HistorySortKey) => void;
  setSortDir: (dir: HistorySortDir) => void;
}

// ── Helpers ───────────────────────────────────────────────────────────────────

const DEFAULT_FILTERS: HistoryFilters = {
  outcome: "all",
  format: "all",
  mode: "all",
  deckName: "all",
  dateFrom: null,
  dateTo: null,
};

function makeRecord(wins: number, losses: number, draws: number): WinLossRecord {
  const total = wins + losses + draws;
  return { wins, losses, draws, total, winRate: total > 0 ? wins / total : 0 };
}

export function computeStats(records: MatchRecord[]): HistoryStats {
  if (records.length === 0) {
    return {
      overall: makeRecord(0, 0, 0),
      byFormat: [],
      byDeck: [],
      byColor: [],
      avgTurnCount: 0,
      avgDurationSec: 0,
      longestWinStreak: 0,
      currentStreak: { type: null, count: 0 },
      mostPlayedFormat: null,
      mostPlayedDeck: null,
    };
  }

  // ── overall ─────────────────────────────────────────────────────────────
  let wins = 0, losses = 0, draws = 0;
  let turnSum = 0, durationSum = 0;
  for (const r of records) {
    if (r.outcome === "win") wins++;
    else if (r.outcome === "loss") losses++;
    else draws++;
    turnSum += r.turnCount;
    durationSum += (r.endedAt - r.startedAt) / 1000;
  }

  // ── by format ────────────────────────────────────────────────────────────
  const formatMap = new Map<string, { w: number; l: number; d: number }>();
  for (const r of records) {
    const entry = formatMap.get(r.format) ?? { w: 0, l: 0, d: 0 };
    if (r.outcome === "win") entry.w++;
    else if (r.outcome === "loss") entry.l++;
    else entry.d++;
    formatMap.set(r.format, entry);
  }
  const byFormat: FormatStats[] = [...formatMap.entries()]
    .map(([format, { w, l, d }]) => ({ format, record: makeRecord(w, l, d) }))
    .sort((a, b) => b.record.total - a.record.total);

  // ── by deck ──────────────────────────────────────────────────────────────
  const deckMap = new Map<
    string,
    { w: number; l: number; d: number; turns: number; dur: number; colors: ManaColor[]; lastAt: number }
  >();
  for (const r of records) {
    const key = r.deckName ?? "(unknown)";
    const entry = deckMap.get(key) ?? { w: 0, l: 0, d: 0, turns: 0, dur: 0, colors: r.deckColors, lastAt: 0 };
    if (r.outcome === "win") entry.w++;
    else if (r.outcome === "loss") entry.l++;
    else entry.d++;
    entry.turns += r.turnCount;
    entry.dur += (r.endedAt - r.startedAt) / 1000;
    if (r.startedAt > entry.lastAt) {
      entry.lastAt = r.startedAt;
      entry.colors = r.deckColors;
    }
    deckMap.set(key, entry);
  }
  const byDeck: DeckStats[] = [...deckMap.entries()]
    .map(([deckName, { w, l, d, turns, dur, colors, lastAt }]) => {
      const total = w + l + d;
      return {
        deckName,
        colors,
        record: makeRecord(w, l, d),
        avgTurns: total > 0 ? turns / total : 0,
        avgDuration: total > 0 ? dur / total : 0,
        lastPlayedAt: lastAt,
      };
    })
    .sort((a, b) => b.record.total - a.record.total);

  // ── by color identity ────────────────────────────────────────────────────
  const colorMap = new Map<ManaColor, { w: number; l: number; d: number }>();
  for (const r of records) {
    for (const color of r.deckColors) {
      const entry = colorMap.get(color) ?? { w: 0, l: 0, d: 0 };
      if (r.outcome === "win") entry.w++;
      else if (r.outcome === "loss") entry.l++;
      else entry.d++;
      colorMap.set(color, entry);
    }
  }
  const colorOrder: ManaColor[] = ["W", "U", "B", "R", "G"];
  const byColor: ColorStats[] = colorOrder
    .filter((c) => colorMap.has(c))
    .map((color) => {
      const { w, l, d } = colorMap.get(color)!;
      return { color, record: makeRecord(w, l, d) };
    });

  // ── streaks (records are newest-first) ──────────────────────────────────
  let longestWinStreak = 0;
  let currentRun = 0;
  let currentRunType: MatchOutcome | null = null;

  // Walk oldest-to-newest for win streak calculation
  const chronological = [...records].reverse();
  let bestStreak = 0;
  let streak = 0;
  for (const r of chronological) {
    if (r.outcome === "win") {
      streak++;
      if (streak > bestStreak) bestStreak = streak;
    } else {
      streak = 0;
    }
  }
  longestWinStreak = bestStreak;

  // Current streak: newest first
  for (const r of records) {
    if (currentRunType === null) {
      currentRunType = r.outcome;
      currentRun = 1;
    } else if (r.outcome === currentRunType) {
      currentRun++;
    } else {
      break;
    }
  }

  // ── most played ──────────────────────────────────────────────────────────
  const mostPlayedFormat = byFormat[0]?.format ?? null;
  const mostPlayedDeck = byDeck[0]?.deckName ?? null;

  return {
    overall: makeRecord(wins, losses, draws),
    byFormat,
    byDeck,
    byColor,
    avgTurnCount: records.length > 0 ? turnSum / records.length : 0,
    avgDurationSec: records.length > 0 ? durationSum / records.length : 0,
    longestWinStreak,
    currentStreak: { type: currentRunType, count: currentRun },
    mostPlayedFormat,
    mostPlayedDeck,
  };
}

export function applyFilters(records: MatchRecord[], filters: HistoryFilters): MatchRecord[] {
  return records.filter((r) => {
    if (filters.outcome !== "all" && r.outcome !== filters.outcome) return false;
    if (filters.format !== "all" && r.format !== filters.format) return false;
    if (filters.mode !== "all" && r.mode !== filters.mode) return false;
    if (filters.deckName !== "all" && r.deckName !== filters.deckName) return false;
    if (filters.dateFrom !== null && r.startedAt < filters.dateFrom) return false;
    if (filters.dateTo !== null && r.startedAt > filters.dateTo) return false;
    return true;
  });
}

export function sortRecords(
  records: MatchRecord[],
  key: HistorySortKey,
  dir: HistorySortDir,
): MatchRecord[] {
  const sign = dir === "asc" ? 1 : -1;
  return [...records].sort((a, b) => {
    switch (key) {
      case "date":
        return sign * (a.startedAt - b.startedAt);
      case "turns":
        return sign * (a.turnCount - b.turnCount);
      case "duration":
        return sign * ((a.endedAt - a.startedAt) - (b.endedAt - b.startedAt));
    }
  });
}

// ── Store ─────────────────────────────────────────────────────────────────────

export const useMatchHistoryStore = create<MatchHistoryState>()((set) => ({
  records: [],
  loaded: false,
  filters: DEFAULT_FILTERS,
  sortKey: "date",
  sortDir: "desc",

  loadAll: async () => {
    const records = await loadMatchHistory();
    set({ records, loaded: true });
  },

  addRecord: async (record) => {
    const updated = await saveMatchRecord(record);
    set({ records: updated });
  },

  removeRecord: async (id) => {
    const updated = await deleteMatchRecord(id);
    set({ records: updated });
  },

  clearAll: async () => {
    await clearMatchHistory();
    set({ records: [] });
  },

  setFilters: (patch) =>
    set((s) => ({ filters: { ...s.filters, ...patch } })),

  resetFilters: () => set({ filters: DEFAULT_FILTERS }),

  setSortKey: (sortKey) => set({ sortKey }),

  setSortDir: (sortDir) => set({ sortDir }),
}));
