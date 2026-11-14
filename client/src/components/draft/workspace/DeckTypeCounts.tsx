import { useTranslation } from "react-i18next";

import { ManaFontIcon } from "../../icons/ManaFontIcon";

export interface DeckTypeCountsProps {
  counts: { creatures: number; lands: number };
  compact?: boolean;
}

export function DeckTypeCounts({ counts, compact = false }: DeckTypeCountsProps) {
  const { t } = useTranslation("draft");
  const textSize = compact ? "text-[0.65625rem]" : "text-sm";
  const iconSize: "xs" | "md" = compact ? "xs" : "md";

  return (
    <output
      data-deck-type-counts
      className={`inline-flex min-h-9 items-center gap-3 font-semibold tabular-nums text-fg ${textSize}`}
    >
      <span
        aria-label={`${counts.creatures} ${t("pool.groups.creature")}`}
        className="inline-flex items-center gap-1"
      >
        <span>{counts.creatures}</span>
        <ManaFontIcon iconClass="ms-creature" fallbackText="C" size={iconSize} className="shrink-0" />
      </span>
      <span
        aria-label={`${counts.lands} ${t("pool.groups.land")}`}
        className="inline-flex items-center gap-1"
      >
        <span>{counts.lands}</span>
        <ManaFontIcon iconClass="ms-land" fallbackText="L" size={iconSize} className="shrink-0" />
      </span>
    </output>
  );
}
