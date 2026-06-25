import { useTranslation } from "react-i18next";

import type { ManaType, ManaUnit } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { groupManaPoolUnits, manaGroupTooltip } from "../../viewmodel/manaPoolGroups.ts";

const EMPTY_MANA: ManaUnit[] = [];

const MANA_COLORS: Record<ManaType, string> = {
  White: "bg-amber-200 text-amber-950 ring-1 ring-amber-50/60 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Blue: "bg-blue-500/90 text-white ring-1 ring-blue-200/25 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Black: "bg-slate-700 text-slate-100 ring-1 ring-white/10 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Red: "bg-rose-500/90 text-white ring-1 ring-rose-200/25 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Green: "bg-emerald-600/90 text-white ring-1 ring-emerald-200/25 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Colorless: "bg-slate-300 text-slate-800 ring-1 ring-white/20 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
};

interface ManaPoolSummaryProps {
  playerId: number;
  size?: "default" | "sm";
}

export function ManaPoolSummary({ playerId, size = "default" }: ManaPoolSummaryProps) {
  const { t } = useTranslation("game");
  const manaUnits = useGameStore(
    (s) => s.gameState?.players[playerId]?.mana_pool.mana ?? EMPTY_MANA,
  );

  // Group fungible units (color, restrictions, grants) so distinctly-restricted
  // mana of the same color renders as separate pills (shared with the payment UI).
  const entries = groupManaPoolUnits(manaUnits);

  if (entries.length === 0) return null;

  return (
    <div className={`flex items-center ${size === "sm" ? "gap-0.5" : "gap-1"}`}>
      {entries.map((group, index) => {
        const title = manaGroupTooltip((k) => t(k), group);
        return (
          <span
            key={index}
            title={title}
            className={`relative inline-flex items-center justify-center rounded-full font-bold tabular-nums ${size === "sm" ? "h-5 min-w-5 px-1 text-[10px]" : "h-6 min-w-6 px-1.5 text-[11px]"} ${MANA_COLORS[group.color]} ${
              group.special ? "ring-2 ring-dashed ring-white/70" : ""
            }`}
          >
            {group.pipIds.length}
            {group.special && (
              <span
                aria-hidden
                className="absolute -top-1 -right-1 h-2 w-2 rounded-full bg-white/90 ring-1 ring-slate-900/40"
              />
            )}
          </span>
        );
      })}
    </div>
  );
}
