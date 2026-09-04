import { useTranslation } from "react-i18next";

import type { DeckColor, DeckColorDistributionEntry } from "../../services/deckCompatibility";

interface ColorDistributionProps {
  distribution: readonly DeckColorDistributionEntry[];
  presentation?: "default" | "compact";
}

const COLORS: Record<DeckColor, { symbol: string; bg: string }> = {
  White: { symbol: "W", bg: "bg-amber-200" },
  Blue: { symbol: "U", bg: "bg-blue-500" },
  Black: { symbol: "B", bg: "bg-gray-700" },
  Red: { symbol: "R", bg: "bg-red-600" },
  Green: { symbol: "G", bg: "bg-green-600" },
};

export function ColorDistribution({
  distribution,
  presentation = "default",
}: ColorDistributionProps) {
  const { t } = useTranslation("deck-builder");
  if (distribution.length === 0) return null;

  const compact = presentation === "compact";
  return (
    <div
      data-color-distribution
      data-color-distribution-presentation={presentation}
      className={compact ? "space-y-0.5" : "space-y-1"}
    >
      <h4 className={compact
        ? "text-[0.6rem] font-semibold uppercase leading-none text-gray-500"
        : "text-xs font-semibold uppercase text-gray-500"}
      >
        {t("manaCurve.colors")}
      </h4>
      <div className={compact ? "flex h-2 overflow-hidden rounded" : "flex h-3 overflow-hidden rounded"}>
        {distribution.map(({ color, percentage, display_percentage }) => {
          const { symbol, bg } = COLORS[color];
          return (
            <div
              key={color}
              className={`${bg} transition-all`}
              style={{ width: `${percentage}%` }}
              title={`${symbol}: ${display_percentage}%`}
            />
          );
        })}
      </div>
      <div className="flex gap-2">
        {distribution.map(({ color, display_percentage }) => {
          const { symbol, bg } = COLORS[color];
          return (
            <span key={color} className="flex items-center gap-1 text-[10px] text-gray-400">
              <span className={`inline-block h-2 w-2 rounded-full ${bg}`} />
              {symbol} {display_percentage}%
            </span>
          );
        })}
      </div>
    </div>
  );
}