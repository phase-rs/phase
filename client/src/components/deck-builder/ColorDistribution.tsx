import { useTranslation } from "react-i18next";

interface ColorDistributionProps {
  colorValues: string[];
  presentation?: "default" | "compact";
}

const COLORS = [
  { symbol: "W", bg: "bg-amber-200" },
  { symbol: "U", bg: "bg-blue-500" },
  { symbol: "B", bg: "bg-gray-700" },
  { symbol: "R", bg: "bg-red-600" },
  { symbol: "G", bg: "bg-green-600" },
] as const;

export function ColorDistribution({
  colorValues,
  presentation = "default",
}: ColorDistributionProps) {
  const { t } = useTranslation("deck-builder");
  const counts = new Map<string, number>();
  let total = 0;

  for (const identity of colorValues) {
    for (const symbol of identity) {
      if (!COLORS.some((color) => color.symbol === symbol)) continue;
      counts.set(symbol, (counts.get(symbol) ?? 0) + 1);
      total += 1;
    }
  }

  if (total === 0) return null;

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
        {COLORS.map(({ symbol, bg }) => {
          const count = counts.get(symbol) ?? 0;
          if (count === 0) return null;
          const percentage = (count / total) * 100;
          return (
            <div
              key={symbol}
              className={`${bg} transition-all`}
              style={{ width: `${percentage}%` }}
              title={`${symbol}: ${Math.round(percentage)}%`}
            />
          );
        })}
      </div>
      <div className="flex gap-2">
        {COLORS.map(({ symbol, bg }) => {
          const count = counts.get(symbol) ?? 0;
          if (count === 0) return null;
          const percentage = Math.round((count / total) * 100);
          return (
            <span key={symbol} className="flex items-center gap-1 text-[10px] text-gray-400">
              <span className={`inline-block h-2 w-2 rounded-full ${bg}`} />
              {symbol} {percentage}%
            </span>
          );
        })}
      </div>
    </div>
  );
}