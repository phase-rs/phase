import {
  BRACKET_LABEL,
  COMMANDER_BRACKETS,
  type CommanderBracket,
} from "../../types/bracket";

interface Props {
  /** Currently-selected brackets. Empty array = filter off (no constraint). */
  selected: CommanderBracket[];
  onChange: (next: CommanderBracket[]) => void;
}

export function BracketFilter({ selected, onChange }: Props) {
  const toggle = (b: CommanderBracket) => {
    onChange(selected.includes(b) ? selected.filter((x) => x !== b) : [...selected, b].sort((a, b) => a - b));
  };

  return (
    <div className="flex flex-wrap gap-1.5" role="group" aria-label="Bracket filter">
      {COMMANDER_BRACKETS.map((b) => {
        const active = selected.includes(b);
        return (
          <button
            key={b}
            type="button"
            aria-pressed={active}
            onClick={() => toggle(b)}
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
