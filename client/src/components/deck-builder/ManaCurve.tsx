interface ManaCurveProps {
  /** CMC values for each card in the deck (one entry per card copy). */
  cmcValues: number[];
  /** Color identity strings (e.g., "W", "U", "B", "R", "G") for each card copy. */
  colorValues: string[];
}

const CMC_LABELS = ["0", "1", "2", "3", "4", "5", "6+"];
const CHART_HEIGHT = 96;
const MAX_BAR_HEIGHT = 72;
const MIN_BAR_HEIGHT = 8;

const COLOR_MAP: Record<string, { bg: string; label: string }> = {
  W: { bg: "bg-amber-200", label: "W" },
  U: { bg: "bg-blue-500", label: "U" },
  B: { bg: "bg-gray-700", label: "B" },
  R: { bg: "bg-red-600", label: "R" },
  G: { bg: "bg-green-600", label: "G" },
};

export function ManaCurve({ cmcValues, colorValues }: ManaCurveProps) {
  // Count cards at each CMC bucket (0 through 6+)
  const buckets = new Array(7).fill(0) as number[];
  for (const cmc of cmcValues) {
    const idx = Math.min(Math.floor(cmc), 6);
    buckets[idx]++;
  }
  const maxCount = Math.max(...buckets, 1);

  // Count color distribution
  const colorCounts: Record<string, number> = {};
  let totalColors = 0;
  for (const color of colorValues) {
    for (const c of color.split("")) {
      if (COLOR_MAP[c]) {
        colorCounts[c] = (colorCounts[c] ?? 0) + 1;
        totalColors++;
      }
    }
  }

  return (
    <div className="space-y-3">
      {/* Mana Curve */}
      <div>
        <h4 className="mb-1 text-xs font-semibold uppercase text-gray-500">
          Mana Curve
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

      {/* Color Distribution */}
      {totalColors > 0 && (
        <div>
          <h4 className="mb-1 text-xs font-semibold uppercase text-gray-500">
            Colors
          </h4>
          <div className="flex h-3 overflow-hidden rounded">
            {Object.entries(COLOR_MAP).map(([color, { bg }]) => {
              const count = colorCounts[color] ?? 0;
              if (count === 0) return null;
              const pct = (count / totalColors) * 100;
              return (
                <div
                  key={color}
                  className={`${bg} transition-all`}
                  style={{ width: `${pct}%` }}
                  title={`${COLOR_MAP[color].label}: ${Math.round(pct)}%`}
                />
              );
            })}
          </div>
          <div className="mt-1 flex gap-2">
            {Object.entries(COLOR_MAP).map(([color, { bg, label }]) => {
              const count = colorCounts[color] ?? 0;
              if (count === 0) return null;
              const pct = Math.round((count / totalColors) * 100);
              return (
                <span key={color} className="flex items-center gap-1 text-[10px] text-gray-400">
                  <span className={`inline-block h-2 w-2 rounded-full ${bg}`} />
                  {label} {pct}%
                </span>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
