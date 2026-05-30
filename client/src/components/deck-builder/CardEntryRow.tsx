import { useState } from "react";
import { useTranslation } from "react-i18next";

import type { DeckEntry } from "../../services/deckParser";
import type { ParsedItem, UnsupportedCard } from "../../services/deckCompatibility";
import { hasAlternatePrintingsSync, resolveOracleIdSync } from "../../services/scryfall";
import { usePrintingsLoaded } from "../../hooks/usePrintingsLoaded";
import { mouseHoverPreview } from "./hoverPreview";

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
  /** "compact" (default) keeps the row controls hover-revealed — used by the
   *  in-game BO3 sideboard modal. "comfortable" makes them always-visible and
   *  touch-sized for the deck builder (hover reveal is invisible on touch). */
  density?: "comfortable" | "compact";
  /** When provided, the alternate-art (✦) badge becomes a tap target that
   *  opens the printing picker — the touch path for art selection (right-click
   *  context menus don't exist on touch). */
  onOpenArtPicker?: (name: string) => void;
  /** Destination name shown on the move button (e.g. "Sideboard",
   *  "Maybeboard", "Main"). When provided, the move control renders as a
   *  labelled "→ {label}" pill so the move target is explicit on touch (where
   *  the title/aria-label tooltip is invisible). When omitted, the control
   *  falls back to a bare directional arrow (used by the BO3 sideboard modal). */
  moveTargetLabel?: string;
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
  density = "compact",
  onOpenArtPicker,
  moveTargetLabel,
}: CardEntryRowProps) {
  const { t } = useTranslation("deck-builder");
  const comfortable = density === "comfortable";
  // Hover-reveal on compact (mouse only); always-visible + larger hit area on
  // comfortable so the controls are usable on touch (~36px on touch, shrinking
  // to the dense hover size on desktop where a pointer is available).
  const controlVisibility = comfortable ? "" : "invisible group-hover:visible";
  const controlSize = comfortable ? "h-9 w-9 text-sm lg:h-7 lg:w-7 lg:text-xs" : "h-5 w-5 text-xs";
  const showCommanderButton =
    section === "main" &&
    !!onSetAsCommander &&
    !!isCommanderEligible &&
    isCommanderEligible(entry.name);
  const [expanded, setExpanded] = useState(false);
  const printingsLoaded = usePrintingsLoaded();
  const oracleId = printingsLoaded ? resolveOracleIdSync(entry.name) : null;
  const hasAlternates = oracleId ? hasAlternatePrintingsSync(oracleId) : false;
  // Labelled "→ {target}" pill when the destination name is known (deck
  // builder); bare directional arrow otherwise (BO3 sideboard modal). The
  // labelled variant widens to fit text, so it overrides the square sizing.
  const moveAriaLabel = moveTargetLabel
    ? t("card.moveToTarget", { name: entry.name, target: moveTargetLabel })
    : section === "main"
      ? t("card.moveToSideboard", { name: entry.name })
      : t("card.moveToMain", { name: entry.name });
  const moveControlSize = moveTargetLabel
    ? comfortable
      ? "h-9 px-2 text-xs lg:h-7"
      : "h-5 px-1.5 text-[11px]"
    : controlSize;

  return (
    <div data-card-name={entry.name.toLowerCase()}>
      <div
        className="group flex items-center justify-between py-0.5 text-sm"
        onContextMenu={(e) => {
          if (onChooseArt) {
            e.preventDefault();
            onChooseArt(entry.name, e.clientX, e.clientY);
          }
        }}
      >
        {/* Tapping the name previews the card — the touch path (hover above is
            mouse-only). The ✦ / ! badges stopPropagation so they keep their
            own actions. */}
        <span
          className={`${unsupported ? "text-amber-200/80" : "text-gray-300"} ${onCardHover ? "cursor-pointer" : ""}`}
          onClick={() => onCardHover?.(entry.name)}
          {...mouseHoverPreview(onCardHover, entry.name)}
        >
          <span className="mr-1 text-gray-500">{entry.count}x</span>
          {entry.name}
          {unsupported && (
            <button
              onClick={(e) => { e.stopPropagation(); setExpanded((v) => !v); }}
              className="ml-1 inline-flex h-3.5 w-3.5 items-center justify-center rounded-sm bg-amber-500/80 text-[8px] font-bold leading-none text-black"
              aria-label={t("card.unsupportedExpand", { count: unsupported.gaps.length })}
              aria-expanded={expanded}
              title={t("card.unsupportedTitle", { count: unsupported.gaps.length })}
            >
              !
            </button>
          )}
          {hasAlternates && (
            onOpenArtPicker ? (
              <button
                type="button"
                onClick={(e) => { e.stopPropagation(); onOpenArtPicker(entry.name); }}
                className="ml-1 inline-flex h-4 w-4 items-center justify-center rounded-sm bg-sky-500/60 text-[9px] leading-none text-sky-100 hover:bg-sky-500/80"
                aria-label={t("card.chooseArtFor", { name: entry.name })}
                title={t("card.alternateArtTap")}
              >
                ✦
              </button>
            ) : (
              <span
                className="ml-1 inline-flex h-3.5 w-3.5 items-center justify-center rounded-sm bg-sky-500/60 text-[9px] leading-none text-sky-100"
                title={t("card.alternateArtRightClick")}
              >
                ✦
              </span>
            )
          )}
        </span>
        <span className="flex items-center">
          {showCommanderButton && (
            <button
              type="button"
              onClick={() => onSetAsCommander?.(entry.name)}
              className={`${controlVisibility} ml-1 ${controlSize} rounded text-purple-300 hover:bg-purple-900/40`}
              aria-label={t("card.makeCommander", { name: entry.name })}
              title={t("card.makeCommanderTitle")}
            >
              ♛
            </button>
          )}
          <button
            type="button"
            onClick={() => onMove(entry.name, section)}
            className={`${controlVisibility} ml-2 ${moveControlSize} inline-flex items-center justify-center gap-0.5 whitespace-nowrap rounded text-sky-300 hover:bg-sky-900/40`}
            aria-label={moveAriaLabel}
            title={moveAriaLabel}
          >
            {moveTargetLabel ? (
              <>
                <span aria-hidden="true">→</span>
                <span>{moveTargetLabel}</span>
              </>
            ) : section === "main" ? (
              "→"
            ) : (
              "←"
            )}
          </button>
          {onRemove && (
            <button
              type="button"
              onClick={() => onRemove(entry.name, section)}
              className={`${controlVisibility} ml-1 ${controlSize} rounded text-red-400 hover:bg-red-900/40`}
              aria-label={t("card.removeOne", { name: entry.name })}
              title={t("card.removeOne", { name: entry.name })}
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
