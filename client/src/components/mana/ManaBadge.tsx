import type { ManaType } from "../../adapter/types.ts";

interface ManaBadgeProps {
  color: ManaType;
  amount: number;
}

const MANA_COLORS: Record<ManaType, string> = {
  White: "bg-yellow-400 text-black",
  Blue: "bg-blue-500 text-white",
  Black: "bg-gray-800 text-white",
  Red: "bg-red-500 text-white",
  Green: "bg-green-600 text-white",
  Colorless: "bg-gray-500 text-white",
};

const MANA_GLOW: Record<ManaType, string> = {
  White: "ring-1 ring-yellow-400/30",
  Blue: "ring-1 ring-blue-400/30",
  Black: "ring-1 ring-gray-400/30",
  Red: "ring-1 ring-red-400/30",
  Green: "ring-1 ring-green-400/30",
  Colorless: "ring-1 ring-gray-300/30",
};

export function ManaBadge({ color, amount }: ManaBadgeProps) {
  if (amount <= 0) return null;

  return (
    <span
      className={`inline-flex h-8 w-8 items-center justify-center rounded-full text-xs font-bold ${MANA_COLORS[color]} ${MANA_GLOW[color]}`}
    >
      {amount}
    </span>
  );
}
