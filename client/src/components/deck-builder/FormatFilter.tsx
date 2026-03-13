export type DeckFormat =
  | "standard"
  | "commander"
  | "modern"
  | "pioneer"
  | "legacy"
  | "vintage"
  | "pauper";

const FORMATS: { value: DeckFormat; label: string }[] = [
  { value: "standard", label: "Standard" },
  { value: "commander", label: "Commander" },
  { value: "modern", label: "Modern" },
  { value: "pioneer", label: "Pioneer" },
  { value: "legacy", label: "Legacy" },
  { value: "vintage", label: "Vintage" },
  { value: "pauper", label: "Pauper" },
];

interface FormatFilterProps {
  selected: DeckFormat;
  onChange: (format: DeckFormat) => void;
}

export function FormatFilter({ selected, onChange }: FormatFilterProps) {
  return (
    <div className="flex flex-wrap gap-1.5">
      {FORMATS.map(({ value, label }) => (
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
