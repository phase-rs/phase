import { useMemo } from "react";

import { useDraftStore } from "../../stores/draftStore";

// ── Types ───────────────────────────────────────────────────────────────

interface ManaCurveProps {
  /** Card names in the current deck selection. */
  cards: string[];
}

// ── Constants ───────────────────────────────────────────────────────────

const CMC_BUCKETS = ["0", "1", "2", "3", "4", "5", "6+"] as const;
const MAX_BAR_HEIGHT = 100;

// ── Component ───────────────────────────────────────────────────────────

/** Simple div-based mana curve bar chart. Per RESEARCH: no chart library needed. */
const EMPTY_POOL: never[] = [];

export function ManaCurve({ cards }: ManaCurveProps) {
  const pool = useDraftStore((s) => s.view?.pool) ?? EMPTY_POOL;

  const counts = useMemo(() => {
    const cmcByName = new Map<string, number>();
    for (const card of pool) {
      cmcByName.set(card.name, card.cmc);
    }

    const buckets = new Map<string, number>();
    for (const bucket of CMC_BUCKETS) buckets.set(bucket, 0);

    for (const name of cards) {
      const cmc = cmcByName.get(name) ?? 0;
      const key = cmc >= 6 ? "6+" : String(cmc);
      buckets.set(key, (buckets.get(key) ?? 0) + 1);
    }

    return CMC_BUCKETS.map((key) => ({
      label: key,
      count: buckets.get(key) ?? 0,
    }));
  }, [cards, pool]);

  const maxCount = Math.max(1, ...counts.map((b) => b.count));

  return (
    <div className="flex flex-col gap-1">
      <div className="text-[10px] font-semibold uppercase tracking-wider text-gray-500">
        Mana Curve
      </div>
      <div className="flex items-end gap-1.5" style={{ height: MAX_BAR_HEIGHT + 24 }}>
        {counts.map(({ label, count }) => (
          <div key={label} className="flex flex-col items-center gap-0.5 flex-1">
            {/* Count label above bar */}
            <span className="text-[10px] text-gray-400 h-4 leading-4">
              {count > 0 ? count : ""}
            </span>
            {/* Bar */}
            <div
              className="w-full bg-blue-500/80 rounded-t transition-all duration-200"
              style={{
                height: count > 0 ? Math.max(4, (count / maxCount) * MAX_BAR_HEIGHT) : 0,
              }}
            />
            {/* CMC label */}
            <span className="text-[10px] text-gray-500">{label}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
