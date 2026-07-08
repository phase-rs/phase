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
  "min-h-9 border border-solid font-semibold transition-colors duration-150 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/35 inline-flex items-center justify-center shadow-[0_1px_0_rgba(255,255,255,0.06)]";

const SIZE_CLASSES: Record<GameButtonSize, string> = {
  xs: "px-2.5 py-1 text-xs rounded-[7px]",
  sm: "px-3.5 py-2 text-sm rounded-[8px]",
  md: "px-3.5 py-2 text-[11px] rounded-[9px] lg:px-4 lg:text-xs",
  lg: "px-6 py-3 text-base rounded-[10px]",
};

const TONE_CLASSES: Record<GameButtonTone, string> = {
  neutral:
    "border-white/12 bg-slate-900/86 text-slate-100 hover:border-white/20 hover:bg-slate-800/88",
  emerald:
    "border-emerald-300/30 bg-emerald-900/56 text-emerald-50 hover:bg-emerald-800/62",
  amber:
    "border-amber-300/30 bg-amber-900/54 text-amber-50 hover:bg-amber-800/62",
  blue: "border-blue-300/30 bg-blue-900/56 text-blue-50 hover:bg-blue-800/62",
  red: "border-red-300/30 bg-red-900/56 text-red-50 hover:bg-red-800/62",
  indigo:
    "border-indigo-300/30 bg-indigo-900/56 text-indigo-50 hover:bg-indigo-800/62",
  slate:
    "border-white/10 bg-slate-900/90 text-slate-100 hover:border-white/20 hover:bg-slate-800/92",
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
