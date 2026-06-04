import { useTranslation } from "react-i18next";

import type { FormatStats } from "../../stores/matchHistoryStore";

interface FormatBreakdownProps {
  formats: FormatStats[];
}

function formatLabel(format: string): string {
  return format.replace(/([A-Z])/g, " $1").trim();
}

interface MiniBarProps {
  winRate: number;
  wins: number;
  losses: number;
  draws: number;
}

function MiniBar({ winRate, wins, losses, draws }: MiniBarProps) {
  const total = wins + losses + draws;
  return (
    <div className="flex h-2 w-full overflow-hidden rounded-full bg-slate-700/50">
      <div className="bg-emerald-500 transition-all" style={{ width: `${winRate * 100}%` }} />
      <div
        className="bg-slate-500/50 transition-all"
        style={{ width: `${(draws / total) * 100}%` }}
      />
      <div
        className="bg-red-600 transition-all"
        style={{ width: `${(losses / total) * 100}%` }}
      />
    </div>
  );
}

export function FormatBreakdown({ formats }: FormatBreakdownProps) {
  const { t } = useTranslation("history");
  if (formats.length === 0) return null;

  return (
    <section>
      <h2 className="mb-4 text-sm font-semibold uppercase tracking-wider text-slate-400">
        {t("byFormat.heading")}
      </h2>

      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
        {formats.map(({ format, record }) => {
          const winPct = Math.round(record.winRate * 100);
          return (
            <div
              key={format}
              className="flex flex-col gap-2 rounded-xl border border-slate-700/40 bg-slate-900/60 px-4 py-3"
            >
              {/* Format name + total */}
              <div className="flex items-baseline justify-between">
                <span className="font-medium text-slate-200">{formatLabel(format)}</span>
                <span className="text-xs text-slate-500">{record.total} games</span>
              </div>

              {/* Win rate bar */}
              <MiniBar
                winRate={record.winRate}
                wins={record.wins}
                losses={record.losses}
                draws={record.draws}
              />

              {/* Numbers */}
              <div className="flex items-center justify-between">
                <span className="text-xs text-slate-500">
                  {record.wins}W / {record.losses}L
                  {record.draws > 0 ? ` / ${record.draws}D` : ""}
                </span>
                <span
                  className={`text-sm font-semibold tabular-nums ${
                    winPct >= 60
                      ? "text-emerald-400"
                      : winPct < 40
                        ? "text-red-400"
                        : "text-slate-300"
                  }`}
                >
                  {winPct}%
                </span>
              </div>
            </div>
          );
        })}
      </div>
    </section>
  );
}
