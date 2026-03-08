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

export function ManaBadge({ color, amount }: ManaBadgeProps) {
  if (amount <= 0) return null;

  return (
    <span
      className={`inline-flex h-7 w-7 items-center justify-center rounded-full text-xs font-bold ${MANA_COLORS[color]}`}
    >
      {amount}
    </span>
  );
}
