import { useTranslation } from "react-i18next";

import type { DeckEntry } from "../../services/deckParser";
import type { UnsupportedCard } from "../../services/deckCompatibility";
import type { GroupAccent } from "./deckGrouping";

import { CardEntryRow } from "./CardEntryRow";

function totalCards(entries: DeckEntry[]): number {
  return entries.reduce((sum, e) => sum + e.count, 0);
}

export interface MoveListProps {
  section: "main" | "sideboard";
  title: string;
  entries: DeckEntry[];
  onMove: (name: string, from: "main" | "sideboard") => void;
  /** Optional — when omitted, rows render without a `-` remove button. See
   *  `CardEntryRowProps.onRemove`. */
  onRemove?: (name: string, section: "main" | "sideboard") => void;
  onCardHover?: (name: string | null) => void;
  unsupportedMap?: Map<string, UnsupportedCard>;
  /** Render the section even when it has zero entries, showing `emptyHint`.
   *  Used for the always-visible sideboard target in the deck editor. */
  alwaysShow?: boolean;
  emptyHint?: string;
  /** Format-specific warning displayed below the title (e.g. "Sideboard
   *  exceeds 15-card limit"). */
  warning?: string;
  onChooseArt?: (cardName: string, x: number, y: number) => void;
  /** Forwarded to each row. See `CardEntryRowProps.onSetAsCommander`. */
  onSetAsCommander?: (name: string) => void;
  isCommanderEligible?: (name: string) => boolean;
  /** Forwarded to each row. Defaults to "compact" so the BO3 sideboard modal
   *  (which renders MoveList directly) is unchanged. See `CardEntryRowProps`. */
  density?: "comfortable" | "compact";
  /** Forwarded to each row's alternate-art badge. See `CardEntryRowProps`. */
  onOpenArtPicker?: (name: string) => void;
  /** Forwarded to each row's move button as the destination label. See
   *  `CardEntryRowProps.moveTargetLabel`. */
  moveTargetLabel?: string;
  /** Optional per-section header accent (leading bar + title tint). Omitted by
   *  the flat sideboard/BO3-modal lists, which keep the plain gray header. */
  accent?: GroupAccent;
}

export function MoveList({
  section,
  title,
  entries,
  onMove,
  onRemove,
  onCardHover,
  unsupportedMap,
  alwaysShow = false,
  emptyHint,
  warning,
  onChooseArt,
  onSetAsCommander,
  isCommanderEligible,
  density = "compact",
  onOpenArtPicker,
  moveTargetLabel,
  accent,
}: MoveListProps) {
  const { t } = useTranslation("deck-builder");
  if (entries.length === 0 && !alwaysShow) return null;
  const count = totalCards(entries);

  return (
    <div className="mb-2">
      <div className="mb-1 flex items-center justify-between text-xs font-semibold uppercase text-gray-500">
        <span className={`flex items-center gap-1.5 ${accent ? accent.text : ""}`}>
          {accent && (
            <span className={`h-3 w-1 shrink-0 rounded-full ${accent.bar}`} aria-hidden="true" />
          )}
          {title}
        </span>
        <span>({count})</span>
      </div>
      {warning && (
        <div
          className="mb-1 rounded border border-amber-500/40 bg-amber-500/10 px-2 py-1 text-[11px] text-amber-200"
          role="alert"
        >
          {warning}
        </div>
      )}
      {entries.length === 0 ? (
        <div className="rounded border border-dashed border-white/10 px-2 py-1.5 text-xs italic text-gray-500">
          {emptyHint ?? t("moveList.empty")}
        </div>
      ) : (
        entries.map((entry) => (
          <CardEntryRow
            key={entry.name}
            entry={entry}
            section={section}
            onMove={onMove}
            onRemove={onRemove}
            onCardHover={onCardHover}
            unsupported={unsupportedMap?.get(entry.name)}
            onChooseArt={onChooseArt}
            onSetAsCommander={onSetAsCommander}
            isCommanderEligible={isCommanderEligible}
            density={density}
            onOpenArtPicker={onOpenArtPicker}
            moveTargetLabel={moveTargetLabel}
          />
        ))
      )}
    </div>
  );
}
