import { useTranslation } from "react-i18next";

import type { ColorStats } from "../../stores/matchHistoryStore";
import { ManaSymbol } from "../mana/ManaSymbol";

interface ColorWinRateProps {
  colors: ColorStats[];
}

const COLOR_BAR_CLASS: Record<string, string> = {
  W: "bg-amber-200",
  U: "bg-blue-500",
  B: "bg-slate-400",
  R: "bg-red-500",
  G: "bg-emerald-600",
};

export function ColorWinRate({ colors }: ColorWinRateProps) {
  const { t } = useTranslation("history");
  if (colors.length === 0) return null;

  return (
    <section>
      <h2 className="mb-4 text-sm font-semibold uppercase tracking-wider text-slate-400">
        {t("byColor.heading")}
      </h2>

      <div className="flex flex-col gap-3 rounded-xl border border-slate-700/40 bg-slate-900/60 p-4">
        {colors.map(({ color, record }) => {
          const barClass = COLOR_BAR_CLASS[color] ?? "bg-slate-500";
          const winPct = Math.round(record.winRate * 100);

          return (
            <div key={color} className="flex items-center gap-3">
              {/* Color pip */}
              <div className="flex w-24 shrink-0 items-center gap-1.5">
                <ManaSymbol shard={color} size="sm" />
                <span className="text-sm text-slate-300">
                  {t(`colorNames.${color as "W" | "U" | "B" | "R" | "G"}`)}
                </span>
              </div>

              {/* Stacked bar */}
              <div className="flex h-3 flex-1 overflow-hidden rounded-full bg-slate-700/50">
                <div
                  className={`transition-all ${barClass}`}
                  style={{ width: `${record.winRate * 100}%` }}
                />
                <div
                  className="bg-slate-500/40 transition-all"
                  style={{ width: `${(record.draws / record.total) * 100}%` }}
                />
                <div
                  className="bg-red-700 transition-all"
                  style={{ width: `${(record.losses / record.total) * 100}%` }}
                />
              </div>

              {/* Labels */}
              <span className={`w-10 text-right text-sm font-semibold tabular-nums ${
                winPct >= 60 ? "text-emerald-400" : winPct < 40 ? "text-red-400" : "text-slate-200"
              }`}>
                {winPct}%
              </span>
              <span className="w-28 text-right text-xs text-slate-500">
                {record.wins}W / {record.losses}L
                {record.draws > 0 ? ` / ${record.draws}D` : ""}
              </span>
            </div>
          );
        })}
      </div>
    </section>
  );
}
