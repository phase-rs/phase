import { useTranslation } from "react-i18next";

import type { PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";

interface NextUpBadgeProps {
  playerId: PlayerId;
  compact?: boolean;
  className?: string;
}

export function NextUpBadge({ playerId, compact = false, className = "" }: NextUpBadgeProps) {
  const { t } = useTranslation("game");
  const isNextUp = useGameStore((s) =>
    s.gameState?.derived?.turn_order?.some(
      (slot) => slot.player === playerId && slot.turns_from_now === 1,
    ) ?? false
  );

  if (!isNextUp) return null;

  const tooltip = t("nextUp.tooltip");

  return (
    <span
      aria-label={tooltip}
      title={tooltip}
      className={`inline-flex shrink-0 items-center justify-center rounded-full border border-amber-200/70 bg-amber-300 px-1.5 font-black uppercase leading-none text-amber-950 shadow-[0_0_8px_rgba(251,191,36,0.4)] ring-1 ring-black/35 ${compact ? "h-3.5 text-[6px] tracking-[0.08em]" : "h-4 text-[7px] tracking-[0.1em]"} ${className}`.trim()}
    >
      {t("nextUp.label")}
    </span>
  );
}
