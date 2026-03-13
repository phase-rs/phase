export type MenuButtonTone = "neutral" | "emerald" | "amber" | "blue" | "red" | "indigo" | "slate" | "cyan";
export type MenuButtonSize = "sm" | "md" | "lg";

const BASE =
  "min-h-11 border border-solid font-medium backdrop-blur-sm transition-colors duration-150 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/30 inline-flex items-center justify-center";

const TONES: Record<MenuButtonTone, string> = {
  neutral: "border-white/12 bg-black/18 text-white/78 hover:border-white/20 hover:bg-white/8 hover:text-white",
  emerald: "border-emerald-300/18 bg-emerald-400/10 text-emerald-100 hover:border-emerald-300/26 hover:bg-emerald-400/14",
  amber: "border-amber-300/18 bg-amber-400/10 text-amber-100 hover:border-amber-300/26 hover:bg-amber-400/14",
  blue: "border-blue-300/18 bg-blue-400/10 text-blue-100 hover:border-blue-300/26 hover:bg-blue-400/14",
  red: "border-red-300/18 bg-red-400/10 text-red-100 hover:border-red-300/26 hover:bg-red-400/14",
  indigo: "border-indigo-300/18 bg-indigo-400/10 text-indigo-100 hover:border-indigo-300/26 hover:bg-indigo-400/14",
  slate: "border-slate-300/16 bg-slate-300/8 text-slate-100 hover:border-slate-300/24 hover:bg-slate-300/12",
  cyan: "border-cyan-300/18 bg-cyan-400/10 text-cyan-100 hover:border-cyan-300/26 hover:bg-cyan-400/14",
};

const SIZES: Record<MenuButtonSize, string> = {
  sm: "px-4 py-2 rounded-xl text-sm",
  md: "px-6 py-3 rounded-[18px] text-base",
  lg: "px-10 py-4 rounded-[22px] text-lg",
};

const DISABLED = "border-white/8 bg-white/5 text-white/30 cursor-not-allowed";

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
