import type { ManaType, ManaUnit } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";

const EMPTY_MANA: ManaUnit[] = [];

const MANA_COLORS: Record<ManaType, string> = {
  White: "bg-amber-200 text-amber-950 ring-1 ring-amber-50/60 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Blue: "bg-blue-500/90 text-white ring-1 ring-blue-200/25 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Black: "bg-slate-700 text-slate-100 ring-1 ring-white/10 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Red: "bg-rose-500/90 text-white ring-1 ring-rose-200/25 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Green: "bg-emerald-600/90 text-white ring-1 ring-emerald-200/25 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Colorless: "bg-slate-300 text-slate-800 ring-1 ring-white/20 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
};

const MANA_ORDER: ManaType[] = ["White", "Blue", "Black", "Red", "Green", "Colorless"];

interface ManaPoolSummaryProps {
  playerId: number;
}

export function ManaPoolSummary({ playerId }: ManaPoolSummaryProps) {
  const manaUnits = useGameStore(
    (s) => s.gameState?.players[playerId]?.mana_pool.mana ?? EMPTY_MANA,
  );

  const counts = new Map<ManaType, number>();
  for (const unit of manaUnits) {
    if (unit.restrictions.includes("ConvokePayment")) continue;
    counts.set(unit.color, (counts.get(unit.color) ?? 0) + 1);
  }

  const entries = MANA_ORDER
    .filter((color) => (counts.get(color) ?? 0) > 0)
    .map((color) => ({ color, count: counts.get(color)! }));

  if (entries.length === 0) return null;

  return (
    <div className="flex items-center gap-1">
      {entries.map(({ color, count }) => (
        <span
          key={color}
          className={`inline-flex h-6 min-w-6 items-center justify-center rounded-full px-1.5 text-[11px] font-bold tabular-nums ${MANA_COLORS[color]}`}
        >
          {count}
        </span>
      ))}
    </div>
  );
}
