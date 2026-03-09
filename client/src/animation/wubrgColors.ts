import type { ManaColor } from "../adapter/types";

export const WUBRG_COLORS: Record<ManaColor | "Colorless", string> = {
  White: "#fbbf24",
  Blue: "#06b6d4",
  Black: "#a855f7",
  Red: "#ef4444",
  Green: "#22c55e",
  Colorless: "#94a3b8",
};

export function getCardColors(colors: ManaColor[]): string[] {
  if (colors.length === 0) {
    return [WUBRG_COLORS.Colorless];
  }
  return colors.map((c) => WUBRG_COLORS[c]);
}
