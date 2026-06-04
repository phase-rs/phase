import { useTranslation } from "react-i18next";

import type { MatchRecord } from "../../services/matchHistoryPersistence";
import type { HistoryFilters as HistoryFiltersState, HistorySortDir, HistorySortKey } from "../../stores/matchHistoryStore";

interface HistoryFiltersProps {
  filters: HistoryFiltersState;
  sortKey: HistorySortKey;
  sortDir: HistorySortDir;
  records: MatchRecord[];
  onFilterChange: (patch: Partial<HistoryFiltersState>) => void;
  onResetFilters: () => void;
  onSortKeyChange: (key: HistorySortKey) => void;
  onSortDirToggle: () => void;
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/** Derive the sorted, deduplicated set of values for a given record field. */
function uniqueSorted(records: MatchRecord[], key: keyof MatchRecord): string[] {
  const values = new Set<string>();
  for (const r of records) {
    const v = r[key];
    if (v != null && v !== "") values.add(String(v));
  }
  return [...values].sort();
}

function formatLabel(format: string): string {
  return format.replace(/([A-Z])/g, " $1").trim();
}

// ── Sub-components ────────────────────────────────────────────────────────────

interface SelectProps {
  label: string;
  value: string;
  onChange: (v: string) => void;
  options: { value: string; label: string }[];
  id: string;
}

function FilterSelect({ label, value, onChange, options, id }: SelectProps) {
  return (
    <div className="flex flex-col gap-1">
      <label htmlFor={id} className="text-[10px] font-medium uppercase tracking-wider text-slate-500">
        {label}
      </label>
      <select
        id={id}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="rounded-md border border-slate-700/60 bg-slate-800/60 px-2.5 py-1.5 text-sm text-slate-200 focus:border-slate-500 focus:outline-none"
      >
        {options.map((opt) => (
          <option key={opt.value} value={opt.value}>
            {opt.label}
          </option>
        ))}
      </select>
    </div>
  );
}

// ── Main component ────────────────────────────────────────────────────────────

export function HistoryFilters({
  filters,
  sortKey,
  sortDir,
  records,
  onFilterChange,
  onResetFilters,
  onSortKeyChange,
  onSortDirToggle,
}: HistoryFiltersProps) {
  const { t } = useTranslation("history");

  const formats = uniqueSorted(records, "format");
  const decks = uniqueSorted(records, "deckName");
  const modes = uniqueSorted(records, "mode");

  const isFiltered =
    filters.outcome !== "all" ||
    filters.format !== "all" ||
    filters.mode !== "all" ||
    filters.deckName !== "all" ||
    filters.dateFrom !== null ||
    filters.dateTo !== null;

  const outcomeOptions = [
    { value: "all", label: t("outcome.all") },
    { value: "win", label: t("outcome.win") },
    { value: "loss", label: t("outcome.loss") },
    { value: "draw", label: t("outcome.draw") },
  ];

  const formatOptions = [
    { value: "all", label: t("filters.allFormats") },
    ...formats.map((f) => ({ value: f, label: formatLabel(f) })),
  ];

  const deckOptions = [
    { value: "all", label: t("filters.allDecks") },
    ...decks.map((d) => ({ value: d, label: d })),
  ];

  const modeOptions = [
    { value: "all", label: t("mode.all") },
    ...modes.map((m) => ({
      value: m,
      label: t(`mode.${m as "ai" | "local" | "online" | "p2p-host" | "p2p-join" | "draft-match"}`),
    })),
  ];

  const sortKeyOptions: { value: HistorySortKey; label: string }[] = [
    { value: "date", label: t("sort.date") },
    { value: "turns", label: t("sort.turns") },
    { value: "duration", label: t("sort.duration") },
  ];

  return (
    <div className="flex flex-wrap items-end gap-3 rounded-xl border border-slate-700/40 bg-slate-900/50 px-4 py-3">
      <FilterSelect
        id="filter-outcome"
        label={t("filters.outcome")}
        value={filters.outcome}
        onChange={(v) => onFilterChange({ outcome: v as HistoryFiltersState["outcome"] })}
        options={outcomeOptions}
      />
      <FilterSelect
        id="filter-format"
        label={t("filters.format")}
        value={filters.format}
        onChange={(v) => onFilterChange({ format: v })}
        options={formatOptions}
      />
      <FilterSelect
        id="filter-mode"
        label={t("filters.mode")}
        value={filters.mode}
        onChange={(v) => onFilterChange({ mode: v as HistoryFiltersState["mode"] })}
        options={modeOptions}
      />
      <FilterSelect
        id="filter-deck"
        label={t("filters.deck")}
        value={filters.deckName}
        onChange={(v) => onFilterChange({ deckName: v })}
        options={deckOptions}
      />

      {/* Sort controls */}
      <div className="flex flex-col gap-1">
        <span className="text-[10px] font-medium uppercase tracking-wider text-slate-500">
          {t("sort.label")}
        </span>
        <div className="flex items-center gap-1">
          <div className="flex overflow-hidden rounded-md border border-slate-700/60">
            {sortKeyOptions.map((opt) => (
              <button
                key={opt.value}
                type="button"
                onClick={() => onSortKeyChange(opt.value)}
                className={`px-2.5 py-1.5 text-sm transition-colors ${
                  sortKey === opt.value
                    ? "bg-slate-600 text-white"
                    : "bg-slate-800/60 text-slate-400 hover:bg-slate-700/40"
                }`}
              >
                {opt.label}
              </button>
            ))}
          </div>
          <button
            type="button"
            onClick={onSortDirToggle}
            title={sortDir === "desc" ? t("sort.desc") : t("sort.asc")}
            className="flex h-[34px] w-[34px] items-center justify-center rounded-md border border-slate-700/60 bg-slate-800/60 text-slate-400 hover:bg-slate-700/40"
          >
            {sortDir === "desc" ? (
              <svg viewBox="0 0 24 24" className="h-4 w-4 fill-current">
                <path d="M19 15l-7 7-7-7h14zM12 2l7 7H5l7-7z" opacity=".3" />
                <path d="M19 15l-7 7-7-7h14z" />
              </svg>
            ) : (
              <svg viewBox="0 0 24 24" className="h-4 w-4 fill-current">
                <path d="M5 9l7-7 7 7H5z" />
                <path d="M5 9l7-7 7 7H5zM5 15l7 7 7-7H5z" opacity=".3" />
              </svg>
            )}
          </button>
        </div>
      </div>

      {/* Reset button */}
      {isFiltered && (
        <button
          type="button"
          onClick={onResetFilters}
          className="self-end rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-1.5 text-sm text-amber-300 hover:bg-amber-500/20"
        >
          {t("filters.reset")}
        </button>
      )}
    </div>
  );
}
