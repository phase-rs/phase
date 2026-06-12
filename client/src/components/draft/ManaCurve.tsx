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
      <div className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
        {t("manaCurve.title")}
      </div>
      <div className="flex items-end gap-1.5" style={{ height: MAX_BAR_HEIGHT + 24 }}>
        {counts.map(({ label, count }) => (
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
            <div
              className="w-full rounded-t bg-cyan-500/60 transition-all duration-200"
              style={{
                height: count > 0 ? Math.max(4, (count / maxCount) * MAX_BAR_HEIGHT) : 0,
              }}
            />
            <span className="text-[10px] text-white/30">{label}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
