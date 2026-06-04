import { useTranslation } from "react-i18next";

import type { MatchRecord } from "../../services/matchHistoryPersistence";

interface TurnDistributionProps {
  records: MatchRecord[];
}

const CHART_HEIGHT = 80;
const MAX_BAR_HEIGHT = 64;
const MIN_BAR_HEIGHT = 4;

/** Bucket boundaries: [1-5], [6-10], [11-15], …, [36-40], [41+] */
const BUCKETS = [5, 10, 15, 20, 25, 30, 35, 40, Infinity];

function bucketLabel(idx: number): string {
  if (idx === BUCKETS.length - 1) return "40+";
  const lo = idx * 5 + 1;
  const hi = BUCKETS[idx];
  return lo === hi ? `${lo}` : `${lo}–${hi}`;
}

function bucketIndex(turns: number): number {
  for (let i = 0; i < BUCKETS.length; i++) {
    if (turns <= BUCKETS[i]) return i;
  }
  return BUCKETS.length - 1;
}

interface TooltipState {
  idx: number;
  count: number;
  wins: number;
  losses: number;
  draws: number;
  x: number;
  y: number;
}

export function TurnDistribution({ records }: TurnDistributionProps) {
  const { t } = useTranslation("history");
  if (records.length === 0) return null;

  // Build buckets: count total + win/loss/draw per bucket
  const counts = BUCKETS.map(() => ({ total: 0, wins: 0, losses: 0, draws: 0 }));
  for (const r of records) {
    const idx = bucketIndex(r.turnCount);
    counts[idx].total++;
    if (r.outcome === "win") counts[idx].wins++;
    else if (r.outcome === "loss") counts[idx].losses++;
    else counts[idx].draws++;
  }

  // Trim trailing empty buckets
  let lastNonZero = counts.length - 1;
  while (lastNonZero > 0 && counts[lastNonZero].total === 0) lastNonZero--;
  const visible = counts.slice(0, lastNonZero + 1);

  const maxCount = Math.max(...visible.map((b) => b.total), 1);

  return (
    <section>
      <h2 className="mb-4 text-sm font-semibold uppercase tracking-wider text-slate-400">
        {t("turnChart.heading")}
      </h2>

      <div className="rounded-xl border border-slate-700/40 bg-slate-900/60 px-4 pb-3 pt-4">
        <div
          className="flex items-end gap-1"
          style={{ height: CHART_HEIGHT }}
          role="img"
          aria-label={t("turnChart.heading")}
        >
          {visible.map((bucket, idx) => {
            if (bucket.total === 0) {
              return (
                <div key={idx} className="flex flex-1 flex-col items-center gap-1">
                  <div style={{ height: MIN_BAR_HEIGHT, width: "100%" }} className="rounded-sm bg-slate-700/20" />
                </div>
              );
            }

            const barH = MIN_BAR_HEIGHT + ((bucket.total / maxCount) * (MAX_BAR_HEIGHT - MIN_BAR_HEIGHT));
            const winH = (bucket.wins / bucket.total) * barH;
            const drawH = (bucket.draws / bucket.total) * barH;
            const lossH = barH - winH - drawH;

            return (
              <div
                key={idx}
                className="group relative flex flex-1 flex-col items-center"
                title={`Turn ${bucketLabel(idx)}: ${bucket.total} games (${bucket.wins}W/${bucket.losses}L${bucket.draws > 0 ? `/${bucket.draws}D` : ""})`}
              >
                {/* Count label on hover */}
                <span className="absolute -top-5 left-1/2 -translate-x-1/2 whitespace-nowrap rounded bg-slate-800 px-1.5 py-0.5 text-[10px] text-slate-200 opacity-0 shadow transition-opacity group-hover:opacity-100">
                  {bucket.total}
                </span>
                {/* Stacked bar */}
                <div
                  className="flex w-full flex-col-reverse overflow-hidden rounded-sm"
                  style={{ height: barH }}
                >
                  <div className="bg-emerald-500/80 transition-all" style={{ height: winH }} />
                  <div className="bg-slate-500/60 transition-all" style={{ height: drawH }} />
                  <div className="bg-red-600/70 transition-all" style={{ height: lossH }} />
                </div>
              </div>
            );
          })}
        </div>

        {/* X axis labels */}
        <div className="mt-1.5 flex items-center gap-1">
          {visible.map((_, idx) => (
            <div key={idx} className="flex-1 text-center text-[9px] text-slate-600">
              {bucketLabel(idx)}
            </div>
          ))}
        </div>
        <div className="mt-1 text-center text-[10px] text-slate-600">
          {t("turnChart.xLabel")}
        </div>

        {/* Legend */}
        <div className="mt-3 flex justify-center gap-4 text-xs">
          <span className="flex items-center gap-1 text-emerald-400">
            <span className="h-2 w-2 rounded-sm bg-emerald-500/80" />
            Wins
          </span>
          <span className="flex items-center gap-1 text-red-400">
            <span className="h-2 w-2 rounded-sm bg-red-600/70" />
            Losses
          </span>
          <span className="flex items-center gap-1 text-slate-400">
            <span className="h-2 w-2 rounded-sm bg-slate-500/60" />
            Draws
          </span>
        </div>
      </div>
    </section>
  );
}
