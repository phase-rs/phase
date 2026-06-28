import type { GameFormat } from "../../adapter/types";
import { DECK_CONSTRUCTION_FORMATS } from "../../data/formatRegistry";

const DECK_BUILDER_FORMATS = DECK_CONSTRUCTION_FORMATS.map(({ format, label }) => ({
  value: format,
  label,
}));

interface FormatFilterProps {
  selected: GameFormat;
  onChange: (format: GameFormat) => void;
}

export function FormatFilter({ selected, onChange }: FormatFilterProps) {
  return (
    <div className="flex flex-wrap gap-1.5">
      {DECK_BUILDER_FORMATS.map(({ value, label }) => (
        <button
          key={value}
          onClick={() => onChange(value)}
          className={`rounded-xl border px-3 py-1.5 text-xs font-medium transition-colors ${
            selected === value
              ? "border-white/18 bg-white/10 text-white"
              : "border-white/8 bg-black/18 text-slate-400 hover:border-white/14 hover:bg-white/6 hover:text-white"
          }`}
        >
          {label}
        </button>
      ))}
    </div>
  );
}
