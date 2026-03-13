export type GameButtonTone =
  | "neutral"
  | "emerald"
  | "amber"
  | "blue"
  | "red"
  | "indigo"
  | "slate";

export type GameButtonSize = "xs" | "sm" | "md" | "lg";

interface GameButtonOptions {
  tone: GameButtonTone;
  size?: GameButtonSize;
  disabled?: boolean;
  className?: string;
}

const BASE_CLASSES =
  "min-h-11 border border-solid font-semibold backdrop-blur-sm transition-colors duration-150 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/40 inline-flex items-center justify-center";

const SIZE_CLASSES: Record<GameButtonSize, string> = {
  xs: "px-2 py-1 text-xs rounded",
  sm: "px-3 py-1.5 text-sm rounded-md",
  md: "px-4 py-2 text-sm rounded-lg",
  lg: "px-6 py-2.5 text-base rounded-lg",
};

const TONE_CLASSES: Record<GameButtonTone, string> = {
  neutral:
    "border-gray-600 bg-gray-700/80 text-gray-100 hover:bg-gray-600/80",
  emerald:
    "border-orange-400/60 bg-orange-600/85 text-orange-50 hover:bg-orange-500/85",
  amber:
    "border-amber-500/60 bg-amber-700/80 text-amber-50 hover:bg-amber-600/80",
  blue: "border-blue-500/60 bg-blue-700/80 text-blue-50 hover:bg-blue-600/80",
  red: "border-red-500/60 bg-red-700/80 text-red-50 hover:bg-red-600/80",
  indigo:
    "border-indigo-500/60 bg-indigo-700/80 text-indigo-50 hover:bg-indigo-600/80",
  slate:
    "border-slate-500/60 bg-slate-700/80 text-slate-50 hover:bg-slate-600/80",
};

export function gameButtonClass({
  tone,
  size = "md",
  disabled = false,
  className = "",
}: GameButtonOptions): string {
  const parts = [BASE_CLASSES, SIZE_CLASSES[size], TONE_CLASSES[tone]];

  if (disabled) {
    parts.push("opacity-40 pointer-events-none");
  }

  if (className) {
    parts.push(className);
  }

  return parts.join(" ");
}
