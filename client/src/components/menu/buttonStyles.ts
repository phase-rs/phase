export type MenuButtonTone = "neutral" | "emerald" | "amber" | "blue" | "red" | "indigo" | "slate" | "cyan";
export type MenuButtonSize = "sm" | "md" | "lg";

const BASE =
  "min-h-11 border border-solid font-semibold backdrop-blur-sm transition-colors duration-150 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/40 inline-flex items-center justify-center";

const TONES: Record<MenuButtonTone, string> = {
  neutral: "border-white/25 bg-white/8 text-white/80 hover:bg-white/14 hover:text-white",
  emerald: "border-emerald-300/70 bg-emerald-500/20 text-emerald-100 hover:bg-emerald-400/28",
  amber: "border-amber-300/70 bg-amber-500/20 text-amber-100 hover:bg-amber-400/30",
  blue: "border-blue-300/70 bg-blue-500/20 text-blue-100 hover:bg-blue-400/30",
  red: "border-red-300/70 bg-red-500/20 text-red-100 hover:bg-red-400/30",
  indigo: "border-indigo-300/70 bg-indigo-500/20 text-indigo-100 hover:bg-indigo-400/30",
  slate: "border-slate-300/60 bg-slate-500/20 text-slate-100 hover:bg-slate-400/28",
  cyan: "border-cyan-300/70 bg-cyan-500/20 text-cyan-100 hover:bg-cyan-400/30",
};

const SIZES: Record<MenuButtonSize, string> = {
  sm: "px-4 py-2 rounded-lg text-sm",
  md: "px-6 py-3 rounded-xl text-base",
  lg: "px-10 py-4 rounded-2xl text-xl",
};

const DISABLED = "border-slate-600/50 bg-slate-800/45 text-white/35 cursor-not-allowed";

interface MenuButtonClassOptions {
  tone: MenuButtonTone;
  size?: MenuButtonSize;
  disabled?: boolean;
  className?: string;
}

export function menuButtonClass({
  tone,
  size = "md",
  disabled = false,
  className,
}: MenuButtonClassOptions): string {
  return [BASE, SIZES[size], disabled ? DISABLED : TONES[tone], className]
    .filter(Boolean)
    .join(" ");
}
