import { useMemo } from "react";
import { useTranslation } from "react-i18next";

import type { DraftCardInstance } from "../../adapter/draft-adapter";

// ── Types ───────────────────────────────────────────────────────────────

interface ManaCurveProps {
  pool: DraftCardInstance[];
  cards: string[];
}

// ── Constants ───────────────────────────────────────────────────────────

const CMC_BUCKETS = ["0", "1", "2", "3", "4", "5", "6+"] as const;
const MAX_BAR_HEIGHT = 100;

// ── Component ───────────────────────────────────────────────────────────

export function ManaCurve({ pool, cards }: ManaCurveProps) {
  const { t } = useTranslation("draft");

  const { allCounts, creatureCounts } = useMemo(() => {
    const cmcByName = new Map<string, number>();
    const isCreatureByName = new Map<string, boolean>();
    for (const card of pool) {
      cmcByName.set(card.name, card.cmc);
      isCreatureByName.set(card.name, card.type_line.toLowerCase().includes("creature"));
    }

    const allBuckets = new Map<string, number>();
    const creatureBuckets = new Map<string, number>();
    for (const bucket of CMC_BUCKETS) {
      allBuckets.set(bucket, 0);
      creatureBuckets.set(bucket, 0);
    }

    for (const name of cards) {
      const cmc = cmcByName.get(name) ?? 0;
      const key = cmc >= 6 ? "6+" : String(cmc);
      allBuckets.set(key, (allBuckets.get(key) ?? 0) + 1);
      if (isCreatureByName.get(name)) {
        creatureBuckets.set(key, (creatureBuckets.get(key) ?? 0) + 1);
      }
    }

    return {
      allCounts: CMC_BUCKETS.map((key) => ({ label: key, count: allBuckets.get(key) ?? 0 })),
      creatureCounts: CMC_BUCKETS.map((key) => ({
        label: key,
        count: creatureBuckets.get(key) ?? 0,
      })),
    };
  }, [cards, pool]);

  const maxCount = Math.max(1, ...allCounts.map((b) => b.count));

  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center justify-between">
        <div className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
          {t("manaCurve.title")}
        </div>
        <div className="flex items-center gap-2">
          <span className="flex items-center gap-1 text-[0.6rem] text-slate-500">
            <span className="inline-block h-2 w-2 rounded-sm bg-cyan-500/60" />
            {t("manaCurve.allCards", "All")}
          </span>
          <span className="flex items-center gap-1 text-[0.6rem] text-slate-500">
            <span className="inline-block h-2 w-2 rounded-sm bg-amber-400/70" />
            {t("manaCurve.creatures", "Creatures")}
          </span>
        </div>
      </div>
      <div className="flex items-end gap-1.5" style={{ height: MAX_BAR_HEIGHT + 24 }}>
        {allCounts.map(({ label, count }, i) => {
          const creatureCount = creatureCounts[i].count;
          const allHeight = count > 0 ? Math.max(4, (count / maxCount) * MAX_BAR_HEIGHT) : 0;
          const creatureHeight =
            creatureCount > 0
              ? Math.max(4, (creatureCount / maxCount) * MAX_BAR_HEIGHT)
              : 0;
          return (
            <div
              key={label}
              role="meter"
              aria-label={t("manaCurve.bucketLabel", { bucket: label })}
              aria-valuemin={0}
              aria-valuemax={maxCount}
              aria-valuenow={count}
              className="flex flex-1 flex-col items-center gap-0.5"
            >
              <span className="h-4 text-[10px] leading-4 text-white/50">
                {count > 0 ? count : ""}
              </span>
              <div className="relative w-full" style={{ height: MAX_BAR_HEIGHT }}>
                <div
                  className="absolute bottom-0 w-full rounded-t bg-cyan-500/40 transition-all duration-200"
                  style={{ height: allHeight }}
                />
                <div
                  className="absolute bottom-0 w-full rounded-t bg-amber-400/70 transition-all duration-200"
                  style={{ height: creatureHeight }}
                />
              </div>
              <span className="text-[10px] text-white/30">{label}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
