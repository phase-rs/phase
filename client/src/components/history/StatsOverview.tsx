import { useTranslation } from "react-i18next";

import type { HistoryStats } from "../../stores/matchHistoryStore";

interface StatsOverviewProps {
  stats: HistoryStats;
}

// ── Sub-components ────────────────────────────────────────────────────────────

interface StatTileProps {
  label: string;
  value: string | number;
  sub?: string;
  tone?: "green" | "red" | "amber" | "default";
}

function StatTile({ label, value, sub, tone = "default" }: StatTileProps) {
  const valueColor = {
    green: "text-emerald-400",
    red: "text-red-400",
    amber: "text-amber-400",
    default: "text-slate-100",
  }[tone];

  return (
    <div className="flex flex-col gap-1 rounded-xl border border-slate-700/40 bg-slate-900/60 px-4 py-3">
      <span className="text-[10px] font-medium uppercase tracking-wider text-slate-500">
        {label}
      </span>
      <span className={`text-2xl font-bold leading-none tabular-nums ${valueColor}`}>
        {value}
      </span>
      {sub && <span className="text-xs text-slate-500">{sub}</span>}
    </div>
  );
}

// ── Win rate ring ─────────────────────────────────────────────────────────────

interface WinRateRingProps {
  wins: number;
  losses: number;
  draws: number;
}

function WinRateRing({ wins, losses, draws }: WinRateRingProps) {
  const total = wins + losses + draws;
  const winRate = total > 0 ? wins / total : 0;
  const lossRate = total > 0 ? losses / total : 0;
  const drawRate = total > 0 ? draws / total : 0;

  const RADIUS = 36;
  const CIRC = 2 * Math.PI * RADIUS;

  // Segments: win (green), loss (red), draw (grey)
  const winLen = winRate * CIRC;
  const lossLen = lossRate * CIRC;
  const drawLen = drawRate * CIRC;

  // Offsets — start from top (rotate -90°)
  const winOffset = 0;
  const lossOffset = winLen;
  const drawOffset = winLen + lossLen;

  return (
    <div className="flex flex-col items-center gap-2">
      <div className="relative h-24 w-24">
        <svg viewBox="0 0 88 88" className="h-full w-full -rotate-90">
          {/* Background track */}
          <circle cx="44" cy="44" r={RADIUS} fill="none" stroke="#1e293b" strokeWidth="10" />
          {/* Win segment */}
          {wins > 0 && (
            <circle
              cx="44"
              cy="44"
              r={RADIUS}
              fill="none"
              stroke="#10b981"
              strokeWidth="10"
              strokeDasharray={`${winLen} ${CIRC - winLen}`}
              strokeDashoffset={-winOffset}
            />
          )}
          {/* Loss segment */}
          {losses > 0 && (
            <circle
              cx="44"
              cy="44"
              r={RADIUS}
              fill="none"
              stroke="#dc2626"
              strokeWidth="10"
              strokeDasharray={`${lossLen} ${CIRC - lossLen}`}
              strokeDashoffset={-lossOffset}
            />
          )}
          {/* Draw segment */}
          {draws > 0 && (
            <circle
              cx="44"
              cy="44"
              r={RADIUS}
              fill="none"
              stroke="#64748b"
              strokeWidth="10"
              strokeDasharray={`${drawLen} ${CIRC - drawLen}`}
              strokeDashoffset={-drawOffset}
            />
          )}
        </svg>
        {/* Center label */}
        <div className="absolute inset-0 flex flex-col items-center justify-center">
          <span className="text-xl font-bold tabular-nums text-slate-100">
            {Math.round(winRate * 100)}%
          </span>
        </div>
      </div>
      {/* Legend */}
      <div className="flex gap-3 text-xs">
        <span className="flex items-center gap-1 text-emerald-400">
          <span className="h-2 w-2 rounded-full bg-emerald-500" />
          {wins}W
        </span>
        <span className="flex items-center gap-1 text-red-400">
          <span className="h-2 w-2 rounded-full bg-red-600" />
          {losses}L
        </span>
        {draws > 0 && (
          <span className="flex items-center gap-1 text-slate-400">
            <span className="h-2 w-2 rounded-full bg-slate-500" />
            {draws}D
          </span>
        )}
      </div>
    </div>
  );
}

// ── Main component ────────────────────────────────────────────────────────────

function formatDurationSec(seconds: number): string {
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const m = Math.floor(seconds / 60);
  const s = Math.round(seconds % 60);
  if (m >= 60) return `${Math.floor(m / 60)}h ${m % 60}m`;
  return `${m}m ${s}s`;
}

export function StatsOverview({ stats }: StatsOverviewProps) {
  const { t } = useTranslation("history");
  const { overall, avgTurnCount, avgDurationSec, longestWinStreak, currentStreak } = stats;

  const streakLabel = (() => {
    if (!currentStreak.type) return t("stats.noStreak");
    const count = currentStreak.count;
    if (currentStreak.type === "win") return t("stats.currentStreakWin", { count });
    if (currentStreak.type === "loss") return t("stats.currentStreakLoss", { count });
    return t("stats.currentStreakDraw", { count });
  })();

  const streakTone = (() => {
    if (!currentStreak.type) return "default" as const;
    if (currentStreak.type === "win") return "green" as const;
    if (currentStreak.type === "loss") return "red" as const;
    return "default" as const;
  })();

  return (
    <section>
      <h2 className="mb-4 text-sm font-semibold uppercase tracking-wider text-slate-400">
        {t("stats.heading")}
      </h2>

      <div className="flex flex-wrap gap-4">
        {/* Win rate ring */}
        <div className="flex items-center justify-center rounded-xl border border-slate-700/40 bg-slate-900/60 px-6 py-4">
          <WinRateRing wins={overall.wins} losses={overall.losses} draws={overall.draws} />
        </div>

        {/* Stat tiles */}
        <div className="grid flex-1 grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4">
          <StatTile
            label={t("stats.winRate")}
            value={`${Math.round(overall.winRate * 100)}%`}
            sub={t("stats.wld", { wins: overall.wins, losses: overall.losses, draws: overall.draws })}
            tone={overall.winRate >= 0.5 ? "green" : overall.winRate < 0.4 ? "red" : "default"}
          />
          <StatTile
            label={t("stats.avgTurns")}
            value={avgTurnCount > 0 ? avgTurnCount.toFixed(1) : "—"}
            tone="default"
          />
          <StatTile
            label={t("stats.avgDuration")}
            value={avgDurationSec > 0 ? formatDurationSec(avgDurationSec) : "—"}
            tone="default"
          />
          <StatTile
            label={t("stats.longestStreak")}
            value={longestWinStreak > 0 ? longestWinStreak : "—"}
            sub={longestWinStreak > 0 ? t("stats.longestStreakValue", { count: longestWinStreak }) : undefined}
            tone={longestWinStreak >= 5 ? "green" : "default"}
          />
          <StatTile
            label={t("stats.currentStreak")}
            value={streakLabel}
            tone={streakTone}
          />
          {stats.mostPlayedFormat && (
            <StatTile
              label={t("stats.mostPlayedFormat")}
              value={stats.mostPlayedFormat.replace(/([A-Z])/g, " $1").trim()}
              tone="default"
            />
          )}
          {stats.mostPlayedDeck && (
            <StatTile
              label={t("stats.mostPlayedDeck")}
              value={stats.mostPlayedDeck}
              tone="default"
            />
          )}
        </div>
      </div>
    </section>
  );
}
