import { useState } from "react";

import type { BracketEstimate, BracketAxis } from "../../adapter/types";
import { BRACKET_TIER_NUMERIC } from "../../adapter/types";
import { BRACKET_LABEL, BRACKET_TIER_CHIP_CLASS, type CommanderBracket } from "../../types/bracket";

interface Props {
  estimate: BracketEstimate | null;
  manualBracket: CommanderBracket | null;
  onCardClick: (cardName: string) => void;
  /** "not-commander" hides the panel; "no-commander" renders the placeholder. */
  emptyReason?: "not-commander" | "no-commander";
}

const AXIS_LABEL: Record<BracketAxis, string> = {
  game_changers: "Game Changers",
  mass_land_denial: "Mass Land Denial",
  extra_turns: "Extra Turns",
  efficient_tutors: "Efficient Tutors",
};

const AXIS_CAPS: Record<BracketAxis, [number, number, number, number, number]> = {
  game_changers: [0, 0, 3, Infinity, Infinity],
  mass_land_denial: [0, 0, 0, Infinity, Infinity],
  extra_turns: [0, 0, Infinity, Infinity, Infinity],
  efficient_tutors: [0, 2, Infinity, Infinity, Infinity],
};

export function BracketAuditPanel({ estimate, manualBracket, onCardClick, emptyReason }: Props) {
  const [expanded, setExpanded] = useState(false);

  if (emptyReason === "not-commander") return null;
  if (emptyReason === "no-commander" || !estimate) {
    return (
      <div className="rounded-md border border-white/10 bg-black/20 px-3 py-2 text-xs text-slate-400">
        Add a commander to see your bracket estimate.
      </div>
    );
  }

  const tierNum = BRACKET_TIER_NUMERIC[estimate.tier];
  const tierLabel = BRACKET_LABEL[tierNum as CommanderBracket];
  const mismatch = manualBracket !== null && manualBracket !== tierNum;

  return (
    <div className="rounded-md border border-white/10 bg-black/20 px-3 py-2">
      <div className="flex flex-wrap items-center gap-1.5">
        <span
          className={`rounded-full border px-2.5 py-1 text-xs font-medium ${BRACKET_TIER_CHIP_CLASS[estimate.tier]}`}
        >
          Estimated: B{tierNum} {tierLabel}
        </span>
        {manualBracket !== null && (
          <span
            className={
              mismatch
                ? "rounded-full border border-amber-300/60 bg-amber-500/20 px-2.5 py-1 text-xs font-medium text-amber-100"
                : "rounded-full border border-white/10 bg-black/20 px-2.5 py-1 text-xs font-medium text-slate-400"
            }
          >
            Manual: B{manualBracket} {BRACKET_LABEL[manualBracket]}
            {mismatch && " ⚠ mismatch"}
          </span>
        )}
        <button
          type="button"
          aria-expanded={expanded}
          aria-label={expanded ? "Hide breakdown" : "Show breakdown"}
          onClick={() => setExpanded((v) => !v)}
          className="ml-auto rounded-full border border-white/10 bg-black/20 px-2.5 py-1 text-xs font-medium text-slate-400 hover:bg-white/6"
        >
          {expanded ? "▲ Hide breakdown" : "▼ Show breakdown"}
        </button>
      </div>

      {expanded && (
        <dl className="mt-3 space-y-2 text-xs">
          {(Object.keys(AXIS_LABEL) as BracketAxis[]).map((axis) => {
            const count = estimate.axes[axis];
            const cards = estimate.contributing[axis];
            const cap = AXIS_CAPS[axis][tierNum - 1];
            return (
              <div key={axis} className="grid grid-cols-[180px_60px_1fr] items-start gap-2">
                <dt className="text-slate-300">{AXIS_LABEL[axis]}</dt>
                <dd className="text-slate-200">
                  {count}
                  {Number.isFinite(cap) && ` / ${cap}`}
                </dd>
                <dd className="text-slate-400">
                  {cards.length === 0 && "—"}
                  {cards.map((name, i) => (
                    <span key={name}>
                      <button
                        type="button"
                        onClick={() => onCardClick(name)}
                        className="text-slate-300 underline-offset-2 hover:underline"
                      >
                        {name}
                      </button>
                      {i < cards.length - 1 && ", "}
                    </span>
                  ))}
                </dd>
              </div>
            );
          })}
          <div className="border-t border-white/5 pt-2 text-[10px] text-slate-500">
            Data: {estimate.data_version} ·{" "}
            <a
              href="https://magic.wizards.com/en/news/announcements/introducing-commander-brackets-beta"
              target="_blank"
              rel="noreferrer"
              className="underline-offset-2 hover:underline"
            >
              About brackets ↗
            </a>
          </div>
        </dl>
      )}
    </div>
  );
}
