import { useMemo } from "react";
import { useTranslation } from "react-i18next";

import type { DraftCardInstance } from "../../adapter/draft-adapter";

// ── Types ───────────────────────────────────────────────────────────────

interface ManaCurveProps {
  pool: DraftCardInstance[];
  cards: string[];
  /**
   * Compact is a presentation-only variant for space-constrained summaries.
   * It deliberately retains the curve's meter semantics and translated title.
   */
  presentation?: "default" | "compact";
}

// ── Constants ───────────────────────────────────────────────────────────

const CMC_BUCKETS = ["0", "1", "2", "3", "4", "5", "6+"] as const;
const MAX_BAR_HEIGHT = 100;
const DEFAULT_CURVE_HEIGHT = MAX_BAR_HEIGHT + 24;
// Compact mode still shows the count and bucket for every meter. 52px gives
// those labels a readable line above and below a shorter landscape bar.
const COMPACT_CURVE_HEIGHT = 52;
const COMPACT_MAX_BAR_HEIGHT = 24;

// ── Component ───────────────────────────────────────────────────────────

export function ManaCurve({ pool, cards, presentation = "default" }: ManaCurveProps) {
  const { t } = useTranslation("draft");
  const compact = presentation === "compact";
  const maxBarHeight = compact ? COMPACT_MAX_BAR_HEIGHT : MAX_BAR_HEIGHT;

  const counts = useMemo(() => {
    const cmcByName = new Map<string, number>();
    for (const card of pool) {
      cmcByName.set(card.name, card.cmc);
    }

    const buckets = new Map<string, number>();
    for (const bucket of CMC_BUCKETS) buckets.set(bucket, 0);

    for (const name of cards) {
      const card = pool.find((entry) => entry.name === name);
      if (card === undefined || /\bland\b/i.test(card.type_line)) continue;
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
    <div
      data-mana-curve-presentation={presentation}
      data-mana-curve-geometry={presentation}
      className={compact ? "flex flex-col gap-0.5" : "flex flex-col gap-1"}
    >
      <div
        data-mana-curve-title
        className={compact
          ? "text-[0.6rem] font-semibold uppercase leading-none tracking-[0.18em] text-slate-500"
          : "text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500"}
      >
        {t("manaCurve.title")}
      </div>
      <div
        data-mana-curve-plot
        className={compact ? "flex items-end gap-1" : "flex items-end gap-1.5"}
        style={{ height: compact ? COMPACT_CURVE_HEIGHT : DEFAULT_CURVE_HEIGHT }}
      >
        {counts.map(({ label, count }) => (
          <div
            key={label}
            role="meter"
            aria-label={t("manaCurve.bucketLabel", { bucket: label })}
            aria-valuemin={0}
            aria-valuemax={maxCount}
            aria-valuenow={count}
            data-mana-curve-meter={label}
            className={compact
              ? "flex h-full flex-1 flex-col items-center gap-px"
              : "flex flex-1 flex-col items-center gap-0.5"}
          >
            <span
              data-mana-curve-count
              className={compact
                ? "h-3 text-[8px] leading-3 text-white/50"
                : "h-4 text-[10px] leading-4 text-white/50"}
            >
              {count > 0 ? count : ""}
            </span>
            <div
              data-mana-curve-bar
              className={compact
                ? "mt-auto w-full rounded-t bg-cyan-500/60 transition-all duration-200"
                : "w-full rounded-t bg-cyan-500/60 transition-all duration-200"}
              style={{
                height: count > 0 ? Math.max(compact ? 2 : 4, (count / maxCount) * maxBarHeight) : 0,
              }}
            />
            <span
              data-mana-curve-bucket
              className={compact ? "h-3 text-[8px] leading-3 text-white/30" : "text-[10px] text-white/30"}
            >
              {label}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
