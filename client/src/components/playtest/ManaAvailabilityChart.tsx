/**
 * Bar chart showing average available mana per turn, drawn from Monte Carlo
 * simulation results. Uses a simple SVG bar chart — no third-party charting lib.
 */
import { useTranslation } from "react-i18next";
import type { TurnAggregate } from "../../services/playtestSession";

interface Props {
  turns: TurnAggregate[];
  /** Which series to show (multiple selectable). */
  showMana?: boolean;
  showLands?: boolean;
  showPlayable?: boolean;
  showHand?: boolean;
}

const CHART_H = 80;
const BAR_GAP = 2;

function BarChart({
  data,
  labels,
  color,
  maxVal,
}: {
  data: number[];
  labels: string[];
  color: string;
  maxVal: number;
}) {
  if (data.length === 0) return null;
  const barW = Math.max(4, Math.floor((200 - data.length * BAR_GAP) / data.length));

  return (
    <svg
      viewBox={`0 0 ${data.length * (barW + BAR_GAP)} ${CHART_H}`}
      className="w-full"
      aria-hidden="true"
    >
      {data.map((val, i) => {
        const h = maxVal > 0 ? Math.max(2, (val / maxVal) * CHART_H) : 2;
        const x = i * (barW + BAR_GAP);
        const y = CHART_H - h;
        return (
          <g key={i}>
            <rect x={x} y={y} width={barW} height={h} fill={color} rx={1} opacity={0.8} />
            {/* Tooltip value label on hover via title */}
            <title>{`${labels[i]}: ${val.toFixed(1)}`}</title>
          </g>
        );
      })}
    </svg>
  );
}

export function ManaAvailabilityChart({
  turns,
  showMana = true,
  showLands = true,
  showPlayable = false,
  showHand = false,
}: Props) {
  const { t } = useTranslation("playtest");

  if (turns.length === 0) {
    return (
      <div className="flex h-24 items-center justify-center text-xs text-slate-500">
        {t("chart.noData")}
      </div>
    );
  }

  const labels = turns.map((t) => `T${t.turnNumber}`);
  const maxMana = Math.max(...turns.map((t) => t.avgAvailableMana), 1);
  const maxLands = Math.max(...turns.map((t) => t.avgLandsInPlay), 1);
  const maxPlay = Math.max(...turns.map((t) => t.avgPlayableCount), 1);
  const maxHand = Math.max(...turns.map((t) => t.avgHandSize), 1);

  const series: Array<{
    show: boolean;
    data: number[];
    color: string;
    label: string;
    maxVal: number;
  }> = [
    {
      show: showMana,
      data: turns.map((t) => t.avgAvailableMana),
      color: "#60a5fa",
      label: t("chart.mana"),
      maxVal: maxMana,
    },
    {
      show: showLands,
      data: turns.map((t) => t.avgLandsInPlay),
      color: "#34d399",
      label: t("chart.lands"),
      maxVal: maxLands,
    },
    {
      show: showPlayable,
      data: turns.map((t) => t.avgPlayableCount),
      color: "#f59e0b",
      label: t("chart.playable"),
      maxVal: maxPlay,
    },
    {
      show: showHand,
      data: turns.map((t) => t.avgHandSize),
      color: "#a78bfa",
      label: t("chart.handSize"),
      maxVal: maxHand,
    },
  ];

  return (
    <div className="space-y-3">
      {series
        .filter((s) => s.show)
        .map((s) => (
          <div key={s.label} className="space-y-1">
            <div className="flex items-center gap-2">
              <span
                className="h-2 w-2 shrink-0 rounded-full"
                style={{ background: s.color }}
              />
              <span className="text-[0.65rem] uppercase tracking-wide text-slate-400">
                {s.label}
              </span>
              <span className="ml-auto text-[0.65rem] text-slate-500">
                {t("chart.avgTurn", {
                  avg: (s.data.reduce((a, b) => a + b, 0) / s.data.length).toFixed(1),
                })}
              </span>
            </div>
            <BarChart
              data={s.data}
              labels={labels}
              color={s.color}
              maxVal={s.maxVal}
            />
            {/* X-axis labels */}
            <div
              className="flex justify-between px-px text-[0.55rem] text-slate-600"
              aria-hidden="true"
            >
              {labels.map((l) => (
                <span key={l}>{l}</span>
              ))}
            </div>
          </div>
        ))}
    </div>
  );
}
