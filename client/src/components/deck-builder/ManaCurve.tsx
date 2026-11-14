import { useTranslation } from "react-i18next";

interface ManaCurveProps {
  /** CMC values for each card in the deck (one entry per card copy). */
  cmcValues: number[];
}

const CMC_LABELS = ["0", "1", "2", "3", "4", "5", "6+"];
const CHART_HEIGHT = 96;
const MAX_BAR_HEIGHT = 72;
const MIN_BAR_HEIGHT = 8;

export function ManaCurve({ cmcValues }: ManaCurveProps) {
  const { t } = useTranslation("deck-builder");
  // Count cards at each CMC bucket (0 through 6+)
  const buckets = new Array(7).fill(0) as number[];
  for (const cmc of cmcValues) {
    const idx = Math.min(Math.floor(cmc), 6);
    buckets[idx]++;
  }
  const maxCount = Math.max(...buckets, 1);

  return (
    <div data-mana-curve className="space-y-3">
      <div>
        <h4 className="mb-1 text-xs font-semibold uppercase text-gray-500">
          {t("manaCurve.title")}
        </h4>
        <div className="flex items-end gap-2" style={{ height: CHART_HEIGHT }}>
          {buckets.map((count, i) => {
            const barHeight = count === 0
              ? 0
              : Math.max(
                Math.round((count / maxCount) * MAX_BAR_HEIGHT),
                MIN_BAR_HEIGHT,
              );
            return (
              <div
                key={i}
                className="flex flex-1 flex-col items-center justify-end"
              >
                <span className="mb-0.5 text-[10px] text-gray-400">
                  {count > 0 ? count : ""}
                </span>
                <div className="flex w-full items-end rounded-t bg-white/5">
                  <div
                    className="w-full rounded-t bg-blue-500 transition-all duration-200"
                    style={{ height: barHeight }}
                  />
                </div>
                <span className="mt-0.5 text-[10px] text-gray-500">
                  {CMC_LABELS[i]}
                </span>
              </div>
            );
          })}
        </div>
      </div>

    </div>
  );
}
