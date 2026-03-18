import type { ManaType, ManaUnit } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";

const EMPTY_MANA: ManaUnit[] = [];

const MANA_COLORS: Record<ManaType, string> = {
  White: "bg-amber-300 text-amber-900 shadow-[0_0_10px_3px_rgba(251,191,36,0.5)]",
  Blue: "bg-blue-500 text-white shadow-[0_0_10px_3px_rgba(59,130,246,0.5)]",
  Black: "bg-gray-700 text-gray-200 shadow-[0_0_10px_3px_rgba(107,114,128,0.5)]",
  Red: "bg-red-500 text-white shadow-[0_0_10px_3px_rgba(239,68,68,0.5)]",
  Green: "bg-green-600 text-white shadow-[0_0_10px_3px_rgba(22,163,74,0.5)]",
  Colorless: "bg-gray-400 text-gray-800 shadow-[0_0_10px_3px_rgba(156,163,175,0.5)]",
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
    counts.set(unit.color, (counts.get(unit.color) ?? 0) + 1);
  }

  const entries = MANA_ORDER
    .filter((color) => (counts.get(color) ?? 0) > 0)
    .map((color) => ({ color, count: counts.get(color)! }));

  if (entries.length === 0) return null;

  return (
    <div className="flex animate-pulse items-center gap-1">
      {entries.map(({ color, count }) => (
        <span
          key={color}
          className={`inline-flex h-6 min-w-6 items-center justify-center rounded-full px-1.5 text-xs font-bold ${MANA_COLORS[color]}`}
        >
          {count}
        </span>
      ))}
    </div>
  );
}
