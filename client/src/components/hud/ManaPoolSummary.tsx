import type { ManaType } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";

const MANA_COLORS: Record<ManaType, string> = {
  White: "bg-amber-300 text-amber-900",
  Blue: "bg-blue-500 text-white",
  Black: "bg-gray-700 text-gray-200",
  Red: "bg-red-500 text-white",
  Green: "bg-green-600 text-white",
  Colorless: "bg-gray-400 text-gray-800",
};

const MANA_ORDER: ManaType[] = ["White", "Blue", "Black", "Red", "Green", "Colorless"];

interface ManaPoolSummaryProps {
  playerId: number;
}

export function ManaPoolSummary({ playerId }: ManaPoolSummaryProps) {
  const manaUnits = useGameStore(
    (s) => s.gameState?.players[playerId]?.mana_pool.mana ?? [],
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
    <div className="flex items-center gap-1">
      {entries.map(({ color, count }) => (
        <span
          key={color}
          className={`inline-flex h-5 min-w-5 items-center justify-center rounded-full px-1 text-[10px] font-bold ${MANA_COLORS[color]}`}
        >
          {count}
        </span>
      ))}
    </div>
  );
}
