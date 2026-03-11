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
    <div className="flex flex-wrap gap-1">
      {FORMATS.map(({ value, label }) => (
        <button
          key={value}
          onClick={() => onChange(value)}
          className={`rounded px-2 py-1 text-xs font-medium transition-colors ${
            selected === value
              ? "bg-blue-600 text-white"
              : "bg-gray-800 text-gray-400 hover:bg-gray-700 hover:text-gray-200"
          }`}
        >
          {label}
        </button>
      ))}
    </div>
  );
}
