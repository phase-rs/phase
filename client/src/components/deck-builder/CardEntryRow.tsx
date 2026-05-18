import { useState } from "react";

import type { DeckEntry } from "../../services/deckParser";
import type { ParsedItem, UnsupportedCard } from "../../services/deckCompatibility";
import { hasAlternatePrintingsSync, resolveOracleIdSync } from "../../services/scryfall";
import { usePrintingsLoaded } from "../../hooks/usePrintingsLoaded";

const CATEGORY_COLORS: Record<string, string> = {
  keyword: "text-sky-400",
  ability: "text-violet-400",
  trigger: "text-amber-400",
  static: "text-teal-400",
  replacement: "text-pink-400",
  cost: "text-orange-400",
};

function ParseItemPill({ item, depth = 0 }: { item: ParsedItem; depth?: number }) {
  return (
    <>
      <span
        className={`inline-flex items-center gap-1 rounded px-1 py-px text-[9px] leading-tight ${
          item.supported
            ? "bg-emerald-500/15 text-emerald-300"
            : "bg-rose-500/15 text-rose-300"
        }`}
        title={item.source_text ?? item.label}
      >
        <span className={`font-semibold uppercase ${CATEGORY_COLORS[item.category] ?? "text-gray-400"}`}>
          {item.category.slice(0, 3)}
        </span>
        <span>{item.label}</span>
        {!item.supported && <span className="text-rose-400">&#x2717;</span>}
      </span>
      {item.children?.map((child, i) => (
        <ParseItemPill key={`${depth}-${i}`} item={child} depth={depth + 1} />
      ))}
    </>
  );
}

export interface CardEntryRowProps {
  entry: DeckEntry;
  section: "main" | "sideboard";
  onMove: (name: string, from: "main" | "sideboard") => void;
  /** Optional — when omitted, the `-` remove button is not rendered.
   *  The BO3 between-games sideboarding modal uses this to enforce a pure
   *  partition UI (cards can only be moved between sections, not removed). */
  onRemove?: (name: string, section: "main" | "sideboard") => void;
  onCardHover?: (cardName: string | null) => void;
  unsupported?: UnsupportedCard;
  onChooseArt?: (cardName: string, x: number, y: number) => void;
  /** When defined and the card is commander-eligible in the current format,
   *  renders a crown button that promotes the card to commander (add/partner
   *  /swap semantics handled by the parent). Only shown in main section. */
  onSetAsCommander?: (name: string) => void;
  /** Eligibility predicate paired with `onSetAsCommander`. The row consults it
   *  per-entry so the parent doesn't have to fan out card-data lookups. */
  isCommanderEligible?: (name: string) => boolean;
}

export function CardEntryRow({
  entry,
  section,
  onMove,
  onRemove,
  onCardHover,
  unsupported,
  onChooseArt,
  onSetAsCommander,
  isCommanderEligible,
}: CardEntryRowProps) {
  const showCommanderButton =
    section === "main" &&
    !!onSetAsCommander &&
    !!isCommanderEligible &&
    isCommanderEligible(entry.name);
  const [expanded, setExpanded] = useState(false);
  const printingsLoaded = usePrintingsLoaded();
  const oracleId = printingsLoaded ? resolveOracleIdSync(entry.name) : null;
  const hasAlternates = oracleId ? hasAlternatePrintingsSync(oracleId) : false;
  const moveLabel = section === "main" ? "→" : "←";
  const moveAriaLabel =
    section === "main"
      ? `Move one ${entry.name} to sideboard`
      : `Move one ${entry.name} to main deck`;

  return (
    <div data-card-name={entry.name.toLowerCase()}>
      <div
        className="group flex items-center justify-between py-0.5 text-sm"
        onMouseEnter={() => onCardHover?.(entry.name)}
        onMouseLeave={() => onCardHover?.(null)}
        onContextMenu={(e) => {
          if (onChooseArt) {
            e.preventDefault();
            onChooseArt(entry.name, e.clientX, e.clientY);
          }
        }}
      >
        <span className={unsupported ? "text-amber-200/80" : "text-gray-300"}>
          <span className="mr-1 text-gray-500">{entry.count}x</span>
          {entry.name}
          {unsupported && (
            <button
              onClick={(e) => { e.stopPropagation(); setExpanded((v) => !v); }}
              className="ml-1 inline-flex h-3.5 w-3.5 items-center justify-center rounded-sm bg-amber-500/80 text-[8px] font-bold leading-none text-black"
              aria-label={`${unsupported.gaps.length} unsupported mechanic(s); expand details`}
              aria-expanded={expanded}
              title={`${unsupported.gaps.length} unsupported mechanic(s) — click to expand`}
            >
              !
            </button>
          )}
          {hasAlternates && (
            <span
              className="ml-1 inline-flex h-3.5 w-3.5 items-center justify-center rounded-sm bg-sky-500/60 text-[9px] leading-none text-sky-100"
              title="Alternate art available — right-click to choose"
            >
              ✦
            </span>
          )}
        </span>
        <span className="flex items-center">
          {showCommanderButton && (
            <button
              onClick={() => onSetAsCommander?.(entry.name)}
              className="invisible ml-1 h-5 w-5 rounded text-xs text-purple-300 hover:bg-purple-900/40 group-hover:visible"
              aria-label={`Make ${entry.name} the commander`}
              title="Make this my commander"
            >
              ♛
            </button>
          )}
          <button
            onClick={() => onMove(entry.name, section)}
            className="invisible ml-2 h-5 w-5 rounded text-xs text-sky-300 hover:bg-sky-900/40 group-hover:visible"
            aria-label={moveAriaLabel}
            title={moveAriaLabel}
          >
            {moveLabel}
          </button>
          {onRemove && (
            <button
              onClick={() => onRemove(entry.name, section)}
              className="invisible ml-1 h-5 w-5 rounded text-xs text-red-400 hover:bg-red-900/40 group-hover:visible"
              aria-label={`Remove one ${entry.name}`}
              title={`Remove one ${entry.name}`}
            >
              -
            </button>
          )}
        </span>
      </div>
      {expanded && unsupported && (
        <div className="mb-1.5 ml-4 mt-0.5 rounded-lg border border-white/6 bg-black/30 px-2 py-1.5">
          {unsupported.oracle_text && (
            <div className="mb-1.5 font-mono text-[10px] leading-snug text-slate-400 italic">
              {unsupported.oracle_text}
            </div>
          )}
          <div className="flex flex-wrap gap-1">
            {(unsupported.parse_details ?? []).map((item, i) => (
              <ParseItemPill key={i} item={item} />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
