import type { GameFormat } from "../../adapter/types";

interface FormatOption {
  format: GameFormat;
  label: string;
  description: string;
  players: string;
  icon: string;
  tone: string;
}

const FORMAT_OPTIONS: FormatOption[] = [
  {
    format: "Standard",
    label: "Standard",
    description: "Classic 1v1 constructed",
    players: "2 players, 20 life",
    icon: "\u2694",
    tone: "indigo",
  },
  {
    format: "Commander",
    label: "Commander",
    description: "Singleton with legendary commander",
    players: "2-4 players, 40 life",
    icon: "\uD83D\uDC51",
    tone: "amber",
  },
  {
    format: "FreeForAll",
    label: "Free-for-All",
    description: "Multiplayer battle royale",
    players: "3-6 players, 20 life",
    icon: "\uD83D\uDD25",
    tone: "red",
  },
  {
    format: "TwoHeadedGiant",
    label: "Two-Headed Giant",
    description: "2v2 team-based format",
    players: "4 players, 30 life",
    icon: "\uD83D\uDEE1",
    tone: "emerald",
  },
];

const TONE_CLASSES: Record<string, { border: string; bg: string; hover: string; text: string }> = {
  indigo: {
    border: "border-indigo-400/50",
    bg: "bg-indigo-500/10",
    hover: "hover:bg-indigo-500/20 hover:border-indigo-400/70",
    text: "text-indigo-200",
  },
  amber: {
    border: "border-amber-400/50",
    bg: "bg-amber-500/10",
    hover: "hover:bg-amber-500/20 hover:border-amber-400/70",
    text: "text-amber-200",
  },
  red: {
    border: "border-red-400/50",
    bg: "bg-red-500/10",
    hover: "hover:bg-red-500/20 hover:border-red-400/70",
    text: "text-red-200",
  },
  emerald: {
    border: "border-emerald-400/50",
    bg: "bg-emerald-500/10",
    hover: "hover:bg-emerald-500/20 hover:border-emerald-400/70",
    text: "text-emerald-200",
  },
};

interface FormatPickerProps {
  onFormatSelect: (format: GameFormat) => void;
}

export function FormatPicker({ onFormatSelect }: FormatPickerProps) {
  return (
    <div className="flex w-full max-w-2xl flex-col items-center gap-6 px-4">
      <h2 className="text-2xl font-bold tracking-tight">Choose Format</h2>
      <div className="grid w-full grid-cols-1 gap-4 sm:grid-cols-2">
        {FORMAT_OPTIONS.map((opt) => {
          const tone = TONE_CLASSES[opt.tone];
          return (
            <button
              key={opt.format}
              onClick={() => onFormatSelect(opt.format)}
              className={`flex flex-col items-start gap-2 rounded-xl border p-5 text-left transition-colors ${tone.border} ${tone.bg} ${tone.hover} cursor-pointer`}
            >
              <div className="flex items-center gap-3">
                <span className="text-2xl">{opt.icon}</span>
                <span className={`text-lg font-semibold ${tone.text}`}>
                  {opt.label}
                </span>
              </div>
              <p className="text-sm text-gray-400">{opt.description}</p>
              <p className="text-xs text-gray-500">{opt.players}</p>
            </button>
          );
        })}
      </div>
    </div>
  );
}
