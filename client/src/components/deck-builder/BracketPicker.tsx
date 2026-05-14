import {
  BRACKET_LABEL,
  COMMANDER_BRACKETS,
  type CommanderBracket,
} from "../../types/bracket";

interface Props {
  value: CommanderBracket | null;
  onChange: (next: CommanderBracket | null) => void;
}

export function BracketPicker({ value, onChange }: Props) {
  return (
    <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label="Deck bracket">
      <button
        type="button"
        aria-pressed={value === null}
        onClick={() => onChange(null)}
        className={
          value === null
            ? "rounded-full border border-slate-300/60 bg-slate-500/30 px-2.5 py-1 text-xs font-medium text-slate-100"
            : "rounded-full border border-white/10 bg-black/20 px-2.5 py-1 text-xs font-medium text-slate-400 hover:bg-white/6"
        }
      >
        Unrated
      </button>
      {COMMANDER_BRACKETS.map((b) => {
        const active = value === b;
        return (
          <button
            key={b}
            type="button"
            aria-pressed={active}
            onClick={() => onChange(b)}
            className={
              active
                ? "rounded-full border border-indigo-300/60 bg-indigo-500/30 px-2.5 py-1 text-xs font-medium text-indigo-100"
                : "rounded-full border border-white/10 bg-black/20 px-2.5 py-1 text-xs font-medium text-slate-400 hover:bg-white/6"
            }
          >
            {b} {BRACKET_LABEL[b]}
          </button>
        );
      })}
    </div>
  );
}
