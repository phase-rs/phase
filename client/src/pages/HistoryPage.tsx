import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { BackButton } from "../components/menu/BackButton";
import { ColorWinRate } from "../components/history/ColorWinRate";
import { FormatBreakdown } from "../components/history/FormatBreakdown";
import { HistoryFilters } from "../components/history/HistoryFilters";
import { MatchList } from "../components/history/MatchList";
import { PerDeckStats } from "../components/history/PerDeckStats";
import { RecentFormBadge } from "../components/history/RecentFormBadge";
import { StatsOverview } from "../components/history/StatsOverview";
import { TurnDistribution } from "../components/history/TurnDistribution";
import {
  applyFilters,
  computeStats,
  sortRecords,
  useMatchHistoryStore,
} from "../stores/matchHistoryStore";

// ── Tab types ─────────────────────────────────────────────────────────────────

type HistoryTab = "games" | "stats";

// ── Clear confirmation modal ──────────────────────────────────────────────────

interface ClearModalProps {
  count: number;
  onConfirm: () => void;
  onCancel: () => void;
}

function ClearModal({ count, onConfirm, onCancel }: ClearModalProps) {
  const { t } = useTranslation("history");
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-sm">
      <div className="w-full max-w-sm rounded-2xl border border-slate-700/60 bg-slate-900 p-6 shadow-2xl">
        <h3 className="mb-2 text-lg font-semibold text-slate-100">
          {t("header.clearConfirmTitle")}
        </h3>
        <p className="mb-6 text-sm text-slate-400">
          {t("header.clearConfirm", { count })}
        </p>
        <div className="flex justify-end gap-3">
          <button
            type="button"
            onClick={onCancel}
            className="rounded-lg border border-slate-700 px-4 py-2 text-sm text-slate-300 hover:bg-slate-800"
          >
            {t("header.clearCancel")}
          </button>
          <button
            type="button"
            onClick={onConfirm}
            className="rounded-lg bg-red-700/80 px-4 py-2 text-sm font-medium text-white hover:bg-red-600"
          >
            {t("header.clearConfirmAction")}
          </button>
        </div>
      </div>
    </div>
  );
}

// ── CSV export ─────────────────────────────────────────────────────────────────

function exportCsv(records: ReturnType<typeof useMatchHistoryStore.getState>["records"]): void {
  const header = ["id", "date", "format", "mode", "outcome", "turnCount", "durationSec", "playerLife", "opponentLife", "deckName", "deckColors", "aiDifficulty"].join(",");
  const rows = records.map((r) => [
    r.id,
    new Date(r.startedAt).toISOString(),
    r.format,
    r.mode,
    r.outcome,
    r.turnCount,
    Math.round((r.endedAt - r.startedAt) / 1000),
    r.playerLife,
    r.opponentLife,
    r.deckName ? `"${r.deckName.replace(/"/g, '""')}"` : "",
    r.deckColors.join("|"),
    r.aiDifficulty ?? "",
  ].join(","));
  const csv = [header, ...rows].join("\n");
  const blob = new Blob([csv], { type: "text/csv;charset=utf-8;" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `phase-match-history-${Date.now()}.csv`;
  a.click();
  URL.revokeObjectURL(url);
}

// ── Main page ──────────────────────────────────────────────────────────────────

export function HistoryPage() {
  const { t } = useTranslation("history");
  const navigate = useNavigate();
  const [tab, setTab] = useState<HistoryTab>("games");
  const [showClearModal, setShowClearModal] = useState(false);

  const {
    records,
    loaded,
    filters,
    sortKey,
    sortDir,
    loadAll,
    removeRecord,
    clearAll,
    setFilters,
    resetFilters,
    setSortKey,
    setSortDir,
  } = useMatchHistoryStore();

  // Load history on mount if not yet loaded
  useEffect(() => {
    if (!loaded) void loadAll();
  }, [loaded, loadAll]);

  // Derived: filtered + sorted records
  const filteredRecords = useMemo(
    () => sortRecords(applyFilters(records, filters), sortKey, sortDir),
    [records, filters, sortKey, sortDir],
  );

  // Derived: stats over ALL records (not just filtered)
  const allStats = useMemo(() => computeStats(records), [records]);

  // Derived: stats over FILTERED records for Statistics tab
  const filteredStats = useMemo(() => computeStats(filteredRecords), [filteredRecords]);

  const recentOutcomes = useMemo(() => records.slice(0, 10).map((r) => r.outcome), [records]);

  const handleDelete = (id: string) => void removeRecord(id);

  const handleClearConfirm = () => {
    void clearAll();
    setShowClearModal(false);
  };

  const handleSortDirToggle = () => setSortDir(sortDir === "desc" ? "asc" : "desc");

  return (
    <div className="min-h-screen bg-gray-950 text-white">
      <BackButton onClick={() => navigate("/")} />

      <div className="mx-auto max-w-5xl px-4 pb-16 pt-16">
        {/* Page header */}
        <div className="mb-6 flex flex-wrap items-start justify-between gap-4">
          <div className="flex flex-col gap-2">
            <h1 className="text-3xl font-bold text-slate-100">{t("page.title")}</h1>
            {records.length > 0 && (
              <RecentFormBadge outcomes={recentOutcomes} count={10} />
            )}
          </div>

          <div className="flex items-center gap-2">
            {records.length > 0 && (
              <>
                <button
                  type="button"
                  onClick={() => exportCsv(records)}
                  className="flex items-center gap-1.5 rounded-lg border border-slate-700/60 bg-slate-800/50 px-3 py-2 text-sm text-slate-300 hover:bg-slate-700/50"
                >
                  <svg viewBox="0 0 24 24" className="h-4 w-4 fill-current">
                    <path d="M19 9h-4V3H9v6H5l7 7 7-7zM5 18v2h14v-2H5z" />
                  </svg>
                  {t("header.exportCsv")}
                </button>
                <button
                  type="button"
                  onClick={() => setShowClearModal(true)}
                  className="flex items-center gap-1.5 rounded-lg border border-red-700/40 bg-red-900/20 px-3 py-2 text-sm text-red-300 hover:bg-red-800/30"
                >
                  <svg viewBox="0 0 24 24" className="h-4 w-4 fill-current">
                    <path d="M7 21c-.55 0-1.02-.196-1.412-.587A1.926 1.926 0 0 1 5 19V6H4V4h5V3h6v1h5v2h-1v13c0 .55-.196 1.021-.587 1.413A1.928 1.928 0 0 1 17 21H7Zm2-4h2V8H9v9Zm4 0h2V8h-2v9Z" />
                  </svg>
                  {t("header.clearHistory")}
                </button>
              </>
            )}
          </div>
        </div>

        {/* Quick stats bar (always visible) */}
        {records.length > 0 && (
          <div className="mb-6 flex flex-wrap gap-4 rounded-xl border border-slate-700/30 bg-slate-900/40 px-4 py-3 text-sm">
            <span className="text-slate-500">
              {t("header.gamesPlayed", { count: records.length })}
            </span>
            <span className="text-emerald-400">{allStats.overall.wins}W</span>
            <span className="text-red-400">{allStats.overall.losses}L</span>
            {allStats.overall.draws > 0 && (
              <span className="text-slate-400">{allStats.overall.draws}D</span>
            )}
            <span className="text-slate-400">
              {Math.round(allStats.overall.winRate * 100)}% win rate
            </span>
            <span className="ml-auto text-slate-500">
              avg {allStats.avgTurnCount.toFixed(1)} turns
            </span>
          </div>
        )}

        {/* Filters (visible on both tabs) */}
        {records.length > 0 && (
          <div className="mb-6">
            <HistoryFilters
              filters={filters}
              sortKey={sortKey}
              sortDir={sortDir}
              records={records}
              onFilterChange={setFilters}
              onResetFilters={resetFilters}
              onSortKeyChange={setSortKey}
              onSortDirToggle={handleSortDirToggle}
            />
          </div>
        )}

        {/* Tab bar */}
        <div className="mb-6 flex gap-0 overflow-hidden rounded-xl border border-slate-700/40">
          <button
            type="button"
            onClick={() => setTab("games")}
            className={`flex-1 px-4 py-2.5 text-sm font-medium transition-colors ${
              tab === "games"
                ? "bg-slate-700 text-white"
                : "bg-slate-900/60 text-slate-400 hover:bg-slate-800/60 hover:text-slate-200"
            }`}
          >
            {t("page.tabGames")}
            {filteredRecords.length > 0 && (
              <span className="ml-2 rounded bg-slate-600/50 px-1.5 py-0.5 text-xs tabular-nums">
                {filteredRecords.length}
              </span>
            )}
          </button>
          <button
            type="button"
            onClick={() => setTab("stats")}
            className={`flex-1 px-4 py-2.5 text-sm font-medium transition-colors ${
              tab === "stats"
                ? "bg-slate-700 text-white"
                : "bg-slate-900/60 text-slate-400 hover:bg-slate-800/60 hover:text-slate-200"
            }`}
          >
            {t("page.tabStats")}
          </button>
        </div>

        {/* Tab content */}
        {tab === "games" && (
          <MatchList
            records={filteredRecords}
            allRecordsCount={records.length}
            onDelete={handleDelete}
            onResetFilters={resetFilters}
          />
        )}

        {tab === "stats" && (
          <div className="flex flex-col gap-10">
            {records.length === 0 ? (
              <div className="flex flex-col items-center gap-3 py-12 text-center">
                <span className="text-lg font-semibold text-slate-300">{t("empty.title")}</span>
                <span className="text-sm text-slate-500">{t("empty.description")}</span>
              </div>
            ) : (
              <>
                <StatsOverview stats={filteredStats} />
                <div className="grid grid-cols-1 gap-8 lg:grid-cols-2">
                  <TurnDistribution records={filteredRecords} />
                  <ColorWinRate colors={filteredStats.byColor} />
                </div>
                <FormatBreakdown formats={filteredStats.byFormat} />
                <PerDeckStats decks={filteredStats.byDeck} />
              </>
            )}
          </div>
        )}
      </div>

      {/* Clear confirmation modal */}
      {showClearModal && (
        <ClearModal
          count={records.length}
          onConfirm={handleClearConfirm}
          onCancel={() => setShowClearModal(false)}
        />
      )}
    </div>
  );
}
