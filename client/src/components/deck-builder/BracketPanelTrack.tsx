import { useTranslation } from "react-i18next";

import {
  BRACKET_TIER_BY_NUMERIC,
  BRACKET_TIER_NUMERIC,
  type CommanderBracket,
  type CommanderBracketTier,
} from "../../types/bracket";

const SEGMENTS = [1, 2, 3, 4, 5] as const;

/** Segment hues match the tier chips in `BRACKET_TIER_CHIP_CLASS`. Each filled
 * segment takes its own tier's hue, not the estimate's. */
const SEGMENT_FILL_CLASS: Record<CommanderBracketTier, string> = {
  exhibition: "bg-slate-500",
  core: "bg-emerald-500",
  upgraded: "bg-indigo-500",
  optimized: "bg-amber-500",
  cedh: "bg-rose-500",
};

interface Props {
  tier: CommanderBracketTier;
  declared: CommanderBracket | null;
}

export function BracketPanelTrack({ tier, declared }: Props) {
  const { t } = useTranslation("deck-builder");
  const estimate = BRACKET_TIER_NUMERIC[tier];

  return (
    <div aria-hidden="true" className="flex gap-1">
      {SEGMENTS.map((n) => {
        const filled = BRACKET_TIER_NUMERIC.core <= n && n <= estimate;
        return (
          <div
            key={n}
            data-segment={n}
            data-filled={filled ? "true" : "false"}
            className="flex min-w-0 flex-1 flex-col items-center gap-0.5 text-[10px] text-slate-400"
          >
            <span className="h-3 leading-3">
              {declared === n ? `${t("bracket.card.youMarker")} ▼` : ""}
            </span>
            <span
              className={`h-1.5 w-full rounded-full ${filled ? SEGMENT_FILL_CLASS[BRACKET_TIER_BY_NUMERIC[n]] : "bg-white/10"}`}
            />
            <span className="h-3 leading-3">
              {estimate === n ? `${t("bracket.card.tierShort", { tier: n })} ▲` : ""}
            </span>
          </div>
        );
      })}
    </div>
  );
}
