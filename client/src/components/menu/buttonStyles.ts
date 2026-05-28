export type MenuButtonTone = "neutral" | "emerald" | "amber" | "blue" | "red" | "indigo" | "slate" | "cyan" | "purple";
export type MenuButtonSize = "icon" | "xs" | "chrome" | "sm" | "md" | "lg";

const BASE =
  "border border-solid font-medium backdrop-blur-sm transition-colors duration-150 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/30 inline-flex items-center justify-center";

const TONES: Record<MenuButtonTone, string> = {
  neutral: "border-white/12 bg-black/18 text-white/78 hover:border-white/20 hover:bg-white/8 hover:text-white",
  emerald: "border-emerald-300/18 bg-emerald-400/10 text-emerald-100 hover:border-emerald-300/26 hover:bg-emerald-400/14",
  amber: "border-amber-300/18 bg-amber-400/10 text-amber-100 hover:border-amber-300/26 hover:bg-amber-400/14",
  blue: "border-blue-300/18 bg-blue-400/10 text-blue-100 hover:border-blue-300/26 hover:bg-blue-400/14",
  red: "border-red-300/18 bg-red-400/10 text-red-100 hover:border-red-300/26 hover:bg-red-400/14",
  indigo: "border-indigo-300/18 bg-indigo-400/10 text-indigo-100 hover:border-indigo-300/26 hover:bg-indigo-400/14",
  slate: "border-slate-300/16 bg-slate-300/8 text-slate-100 hover:border-slate-300/24 hover:bg-slate-300/12",
  cyan: "border-cyan-300/18 bg-cyan-400/10 text-cyan-100 hover:border-cyan-300/26 hover:bg-cyan-400/14",
  purple: "border-purple-300/18 bg-purple-400/10 text-purple-100 hover:border-purple-300/26 hover:bg-purple-400/14",
};

// Ghost = tertiary action: no border/fill at rest, tone-tinted text, faint hover wash.
const GHOST_BASE = "border-transparent bg-transparent hover:bg-white/[0.06]";

const GHOST_TONES: Record<MenuButtonTone, string> = {
  neutral: "text-white/55 hover:text-white/85",
  emerald: "text-emerald-300 hover:text-emerald-200",
  amber: "text-amber-300 hover:text-amber-200",
  blue: "text-blue-300 hover:text-blue-200",
  red: "text-red-300 hover:text-red-200",
  indigo: "text-indigo-300 hover:text-indigo-200",
  slate: "text-slate-300 hover:text-slate-200",
  cyan: "text-cyan-300 hover:text-cyan-200",
  purple: "text-purple-300 hover:text-purple-200",
};

const SIZES: Record<MenuButtonSize, string> = {
  icon: "min-h-8 h-8 w-8 p-0 rounded-[10px] text-base",
  xs: "min-h-8 px-2.5 py-1 rounded-lg text-xs",
  // Chrome cluster (FullscreenButton/VolumeControl/AccountControl peers):
  // a true 36×36 square — `min-h-9 h-9 min-w-9` together prevent any caller's
  // `h-*` override from being defeated by an upstream `min-h-*` baseline.
  chrome: "min-h-9 h-9 min-w-9 px-1.5 py-0 rounded-[12px] text-sm",
  sm: "min-h-11 px-4 py-2 rounded-xl text-sm",
  md: "min-h-11 px-6 py-3 rounded-[18px] text-base",
  lg: "min-h-11 px-10 py-4 rounded-[22px] text-lg",
};

const DISABLED = "border-white/8 bg-white/5 text-white/30 cursor-not-allowed";
const GHOST_DISABLED = "border-transparent bg-transparent text-white/25 cursor-not-allowed";

interface MenuButtonClassOptions {
  tone: MenuButtonTone;
  size?: MenuButtonSize;
  ghost?: boolean;
  disabled?: boolean;
  className?: string;
}

export function menuButtonClass({
  tone,
  size = "md",
  ghost = false,
  disabled = false,
  className,
}: MenuButtonClassOptions): string {
  let appearance: string;
  if (disabled) {
    appearance = ghost ? GHOST_DISABLED : DISABLED;
  } else if (ghost) {
    appearance = `${GHOST_BASE} ${GHOST_TONES[tone]}`;
  } else {
    appearance = TONES[tone];
  }
  return [BASE, SIZES[size], appearance, className].filter(Boolean).join(" ");
}
