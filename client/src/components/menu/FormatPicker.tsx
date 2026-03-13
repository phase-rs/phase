import type { GameFormat } from "../../adapter/types";

interface FormatOption {
  format: GameFormat;
  label: string;
  description: string;
  players: string;
  tone: string;
}

const FORMAT_OPTIONS: FormatOption[] = [
  {
    format: "Standard",
    label: "Standard",
    description: "Classic 1v1 constructed",
    players: "2 players, 20 life",
    tone: "indigo",
  },
  {
    format: "Commander",
    label: "Commander",
    description: "Singleton with legendary commander",
    players: "2-4 players, 40 life",
    tone: "amber",
  },
  {
    format: "FreeForAll",
    label: "Free-for-All",
    description: "Multiplayer battle royale",
    players: "3-6 players, 20 life",
    tone: "red",
  },
  {
    format: "TwoHeadedGiant",
    label: "Two-Headed Giant",
    description: "2v2 team-based format",
    players: "4 players, 30 life",
    tone: "emerald",
  },
];

const TONE_CLASSES: Record<string, { accent: string; border: string; bg: string; hover: string; text: string }> = {
  indigo: {
    accent: "bg-indigo-300/70",
    border: "border-white/10",
    bg: "bg-[linear-gradient(180deg,rgba(76,105,255,0.06),rgba(9,13,24,0.82))]",
    hover: "hover:border-white/18 hover:bg-[linear-gradient(180deg,rgba(76,105,255,0.1),rgba(9,13,24,0.9))]",
    text: "text-white",
  },
  amber: {
    accent: "bg-amber-300/70",
    border: "border-white/10",
    bg: "bg-[linear-gradient(180deg,rgba(255,196,122,0.06),rgba(9,13,24,0.82))]",
    hover: "hover:border-white/18 hover:bg-[linear-gradient(180deg,rgba(255,196,122,0.1),rgba(9,13,24,0.9))]",
    text: "text-white",
  },
  red: {
    accent: "bg-red-300/70",
    border: "border-white/10",
    bg: "bg-[linear-gradient(180deg,rgba(248,113,113,0.06),rgba(9,13,24,0.82))]",
    hover: "hover:border-white/18 hover:bg-[linear-gradient(180deg,rgba(248,113,113,0.1),rgba(9,13,24,0.9))]",
    text: "text-white",
  },
  emerald: {
    accent: "bg-emerald-300/70",
    border: "border-white/10",
    bg: "bg-[linear-gradient(180deg,rgba(52,211,153,0.06),rgba(9,13,24,0.82))]",
    hover: "hover:border-white/18 hover:bg-[linear-gradient(180deg,rgba(52,211,153,0.1),rgba(9,13,24,0.9))]",
    text: "text-white",
  },
};

interface FormatPickerProps {
  onFormatSelect: (format: GameFormat) => void;
}

export function FormatPicker({ onFormatSelect }: FormatPickerProps) {
  return (
    <div className="flex w-full max-w-3xl flex-col items-center gap-6 px-4">
      <h2 className="menu-display text-[2rem] leading-tight text-white">Choose Format</h2>
      <div className="grid w-full grid-cols-1 gap-4 sm:grid-cols-2">
        {FORMAT_OPTIONS.map((opt) => {
          const tone = TONE_CLASSES[opt.tone];
          return (
            <button
              key={opt.format}
              onClick={() => onFormatSelect(opt.format)}
              className={`group relative flex min-h-40 flex-col overflow-hidden rounded-[22px] border p-5 text-left transition-colors ${tone.border} ${tone.bg} ${tone.hover} cursor-pointer`}
            >
              <div className={`absolute inset-y-5 left-0 w-[3px] rounded-r ${tone.accent}`} />
              <div className="flex h-full flex-col">
                <div className={`text-[1.35rem] font-semibold ${tone.text}`}>
                  {opt.label}
                </div>
                <p className="mt-2 text-sm leading-6 text-slate-400">{opt.description}</p>
                <p className="mt-auto pt-5 text-sm text-slate-500">{opt.players}</p>
              </div>
            </button>
          );
        })}
      </div>
    </div>
  );
}
