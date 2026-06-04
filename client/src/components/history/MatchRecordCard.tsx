import { useState } from "react";
import { useTranslation } from "react-i18next";

import type { MatchRecord } from "../../services/matchHistoryPersistence";
import { ManaSymbol } from "../mana/ManaSymbol";

interface MatchRecordCardProps {
  record: MatchRecord;
  onDelete?: (id: string) => void;
}

const OUTCOME_STYLES = {
  win: {
    border: "border-emerald-600/40",
    badge: "bg-emerald-600/20 text-emerald-300 border border-emerald-500/40",
    indicator: "bg-emerald-500",
  },
  loss: {
    border: "border-red-700/40",
    badge: "bg-red-700/20 text-red-300 border border-red-600/40",
    indicator: "bg-red-600",
  },
  draw: {
    border: "border-slate-600/30",
    badge: "bg-slate-600/20 text-slate-300 border border-slate-500/30",
    indicator: "bg-slate-500",
  },
};

/** Format a duration in seconds into a human-readable string. */
function formatDuration(seconds: number): string {
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.round(seconds % 60);
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m ${s}s`;
}

/** Format an epoch timestamp as a relative or absolute date. */
function formatDate(ts: number): string {
  const now = Date.now();
  const diff = now - ts;
  const minutes = Math.floor(diff / 60000);
  const hours = Math.floor(diff / 3600000);
  const days = Math.floor(diff / 86400000);
  if (minutes < 2) return "just now";
  if (minutes < 60) return `${minutes}m ago`;
  if (hours < 24) return `${hours}h ago`;
  if (days < 7) return `${days}d ago`;
  return new Date(ts).toLocaleDateString(undefined, { month: "short", day: "numeric", year: days > 365 ? "numeric" : undefined });
}

/** Format a GameFormat PascalCase string for display, inserting spaces. */
function formatLabel(format: string): string {
  return format.replace(/([A-Z])/g, " $1").trim();
}

export function MatchRecordCard({ record, onDelete }: MatchRecordCardProps) {
  const { t } = useTranslation("history");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const styles = OUTCOME_STYLES[record.outcome];
  const durationSec = (record.endedAt - record.startedAt) / 1000;

  return (
    <article
      className={`group relative flex items-stretch gap-3 rounded-lg border bg-white/[0.025] p-3 transition-colors hover:bg-white/[0.04] ${styles.border}`}
    >
      {/* Colored left bar indicating outcome */}
      <div className={`w-1 shrink-0 self-stretch rounded-full ${styles.indicator}`} />

      {/* Main content */}
      <div className="min-w-0 flex-1">
        {/* Top row: outcome badge + format + date */}
        <div className="flex flex-wrap items-center gap-2">
          <span className={`rounded px-2 py-0.5 text-xs font-semibold ${styles.badge}`}>
            {t(`outcome.${record.outcome}`)}
          </span>
          <span className="text-sm font-medium text-slate-200">
            {formatLabel(record.format)}
          </span>
          <span className="ml-auto shrink-0 text-xs text-slate-500">
            {formatDate(record.startedAt)}
          </span>
        </div>

        {/* Second row: deck name + colors + mode */}
        <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1">
          <div className="flex min-w-0 items-center gap-1.5">
            {record.deckColors.length > 0 && (
              <span className="flex items-center gap-0.5">
                {record.deckColors.map((c) => (
                  <ManaSymbol key={c} shard={c} size="xs" />
                ))}
              </span>
            )}
            <span className="truncate text-sm text-slate-300">
              {record.deckName ?? t("record.unknownDeck")}
            </span>
          </div>
          <span className="rounded bg-slate-700/40 px-1.5 py-0.5 text-xs text-slate-400">
            {t(`mode.${record.mode}`)}
          </span>
          {record.aiDifficulty && (
            <span className="text-xs text-slate-500">
              {record.aiDifficulty}
            </span>
          )}
        </div>

        {/* Third row: turn count, duration, life totals */}
        <div className="mt-1.5 flex flex-wrap gap-x-4 gap-y-0.5 text-xs text-slate-500">
          <span>
            {t("record.turns", { count: record.turnCount })}
          </span>
          <span>{formatDuration(durationSec)}</span>
          <span>
            {record.playerLife} → {record.opponentLife} life
          </span>
          {record.playerCount > 2 && (
            <span>{t("record.players", { count: record.playerCount })}</span>
          )}
          {record.commanderName && (
            <span className="italic">{record.commanderName}</span>
          )}
          {(record.mulliganCount ?? 0) > 0 && (
            <span className="text-amber-400/70">
              {t("record.mulligan", { count: record.mulliganCount ?? 1 })}
            </span>
          )}
        </div>
      </div>

      {/* Delete button */}
      {onDelete && (
        <div className="flex shrink-0 items-center">
          {confirmDelete ? (
            <div className="flex gap-1">
              <button
                type="button"
                onClick={() => onDelete(record.id)}
                className="rounded bg-red-700/60 px-2 py-1 text-xs text-red-200 hover:bg-red-600/60"
              >
                Delete
              </button>
              <button
                type="button"
                onClick={() => setConfirmDelete(false)}
                className="rounded bg-slate-700/40 px-2 py-1 text-xs text-slate-300 hover:bg-slate-600/40"
              >
                Cancel
              </button>
            </div>
          ) : (
            <button
              type="button"
              aria-label={t("record.deleteTip")}
              title={t("record.deleteTip")}
              onClick={() => setConfirmDelete(true)}
              className="rounded p-1 text-slate-600 opacity-0 transition-opacity group-hover:opacity-100 hover:text-red-400"
            >
              <svg viewBox="0 0 24 24" className="h-4 w-4 fill-current">
                <path d="M7 21c-.55 0-1.02-.196-1.412-.587A1.926 1.926 0 0 1 5 19V6H4V4h5V3h6v1h5v2h-1v13c0 .55-.196 1.021-.587 1.413A1.928 1.928 0 0 1 17 21H7Zm2-4h2V8H9v9Zm4 0h2V8h-2v9Z" />
              </svg>
            </button>
          )}
        </div>
      )}
    </article>
  );
}
