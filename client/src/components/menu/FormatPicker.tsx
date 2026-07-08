import { useMemo } from "react";

import type { FormatGroup as EngineFormatGroup, FormatMetadata, GameFormat } from "../../adapter/types";
import { FORMAT_REGISTRY } from "../../data/formatRegistry";

interface FormatOption {
  format: GameFormat;
  label: string;
  description: string;
}

interface FormatGroup {
  label: EngineFormatGroup;
  tone: string;
  formats: FormatOption[];
}

// Map the engine's FormatGroup taxonomy to display tones. Engine adds a new
// group → TS exhaustiveness check here forces us to assign a tone.
const GROUP_TONE: Record<EngineFormatGroup, string> = {
  Constructed: "indigo",
  Commander: "amber",
  Limited: "emerald",
  Multiplayer: "emerald",
};

// Render order for groups; mirrors how players think about the game's
// format hierarchy (sanctioned → Commander → Limited → casual).
const GROUP_ORDER: EngineFormatGroup[] = ["Constructed", "Commander", "Limited", "Multiplayer"];

const GROUP_TONES: Record<string, { kicker: string; border: string; bg: string; hover: string }> = {
  indigo: {
    kicker: "text-indigo-300/60",
    border: "border-white/10",
    bg: "bg-[linear-gradient(180deg,rgba(255,255,255,0.045),rgba(9,13,24,0.82))]",
    hover: "hover:border-white/20 hover:bg-[linear-gradient(180deg,rgba(255,255,255,0.07),rgba(9,13,24,0.88))]",
  },
  amber: {
    kicker: "text-amber-300/60",
    border: "border-white/10",
    bg: "bg-[linear-gradient(180deg,rgba(255,255,255,0.045),rgba(9,13,24,0.82))]",
    hover: "hover:border-white/20 hover:bg-[linear-gradient(180deg,rgba(255,255,255,0.07),rgba(9,13,24,0.88))]",
  },
  emerald: {
    kicker: "text-emerald-300/60",
    border: "border-white/10",
    bg: "bg-[linear-gradient(180deg,rgba(255,255,255,0.045),rgba(9,13,24,0.82))]",
    hover: "hover:border-white/20 hover:bg-[linear-gradient(180deg,rgba(255,255,255,0.07),rgba(9,13,24,0.88))]",
  },
};

interface FormatPickerProps {
  onFormatSelect: (format: GameFormat) => void;
  formats?: readonly FormatMetadata[];
}

export function FormatPicker({ onFormatSelect, formats = FORMAT_REGISTRY }: FormatPickerProps) {
  const formatGroups: FormatGroup[] = useMemo(
    () =>
      GROUP_ORDER.map((group) => ({
        label: group,
        tone: GROUP_TONE[group],
        formats: formats.filter((m) => m.group === group).map((m) => ({
          format: m.format,
          label: m.label,
          description: m.description,
        })),
      })).filter((g) => g.formats.length > 0),
    [formats],
  );

  return (
    <div className="flex w-full max-w-3xl flex-col gap-6 sm:gap-8">
      {formatGroups.map((group) => {
        const tone = GROUP_TONES[group.tone];
        return (
          <div key={group.label} className="flex flex-col gap-2.5 sm:gap-3">
            <span className={`text-[0.68rem] uppercase tracking-[0.22em] ${tone.kicker}`}>
              {group.label}
            </span>
            <div className="grid grid-cols-1 gap-2.5 sm:grid-cols-2 sm:gap-3 lg:grid-cols-3 xl:grid-cols-4">
              {group.formats.map((opt) => (
                <button
                  key={opt.format}
                  onClick={() => onFormatSelect(opt.format)}
                  className={`group relative flex flex-col overflow-hidden rounded-[8px] border px-4 py-3.5 text-left transition-colors sm:py-4 ${tone.border} ${tone.bg} ${tone.hover} cursor-pointer`}
                >
                  <div className="text-base font-semibold text-white sm:text-[1.05rem]">
                    {opt.label}
                  </div>
                  <p className="mt-1 text-[0.78rem] leading-5 text-slate-400">
                    {opt.description}
                  </p>
                </button>
              ))}
            </div>
          </div>
        );
      })}
    </div>
  );
}
