import { memo } from "react";

import type {
  GameLogEntry,
  LogCategory,
  LogSegment,
  ObjectId,
  PlayerId,
} from "../../adapter/types.ts";
import { getSeatColor } from "../../hooks/useSeatColor.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { assertNever } from "../../utils/assertNever.ts";
import {
  importanceClass,
  logPresentation,
  toneClass,
} from "../../viewmodel/logFormatting.ts";

const CATEGORY_GLYPHS: Record<LogCategory, string> = {
  Game: "◆",
  Turn: "◷",
  Stack: "↑",
  Combat: "⚔",
  Zone: "↔",
  Life: "♥",
  Mana: "✦",
  State: "◈",
  Token: "◉",
  Trigger: "⚡",
  Special: "★",
  Destroy: "×",
  Debug: "◌",
};

interface LogEntryProps {
  entry: GameLogEntry;
  categoryLabel?: string;
  showCategoryLabel?: boolean;
  onInspectObjectSticky?: (objectId: ObjectId, fallbackCardName?: string) => void;
}

function renderSegment(
  segment: LogSegment,
  index: number,
  seatOrder: PlayerId[] | undefined,
  onInspectObjectSticky?: (objectId: ObjectId, fallbackCardName?: string) => void,
) {
  switch (segment.type) {
    case "Text":
      return <span key={index}>{segment.value}</span>;
    case "CardName":
      return onInspectObjectSticky ? (
        <button
          key={index}
          type="button"
          data-segment="CardName"
          onClick={() => onInspectObjectSticky(segment.value.object_id, segment.value.name)}
          className="-my-3 inline-flex min-h-11 min-w-11 items-center justify-center rounded-sm align-middle font-bold text-yellow-200 underline decoration-yellow-500/50 underline-offset-2 transition hover:text-yellow-100 focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-300"
        >
          {segment.value.name}
        </button>
      ) : (
        <span key={index} data-segment="CardName" className="font-bold text-yellow-200">
          {segment.value.name}
        </span>
      );
    case "PlayerName":
      return (
        <span
          key={index}
          data-segment="PlayerName"
          className="font-bold"
          style={{ color: getSeatColor(segment.value.player_id, seatOrder) }}
        >
          {segment.value.name}
        </span>
      );
    case "Number":
      return (
        <span
          key={index}
          data-segment="Number"
          className="mx-0.5 inline-flex min-w-5 items-center justify-center rounded bg-white/10 px-1 py-px font-bold leading-none tabular-nums text-white ring-1 ring-inset ring-white/20"
        >
          {segment.value}
        </span>
      );
    case "Zone":
      return (
        <span
          key={index}
          data-segment="Zone"
          className="mx-0.5 inline-flex items-center rounded bg-sky-950/70 px-1.5 py-px text-[0.8em] font-semibold uppercase tracking-wide text-sky-200 ring-1 ring-inset ring-sky-700/50"
        >
          {segment.value}
        </span>
      );
    case "Keyword":
      return (
        <span
          key={index}
          data-segment="Keyword"
          className="mx-0.5 inline-flex items-center rounded bg-violet-950/70 px-1.5 py-px text-[0.8em] font-semibold text-violet-200 ring-1 ring-inset ring-violet-700/50"
        >
          {segment.value}
        </span>
      );
    case "Mana":
      return (
        <span
          key={index}
          data-segment="Mana"
          className="mx-0.5 inline-flex items-center rounded-full bg-amber-950/70 px-1.5 py-px text-[0.8em] font-bold text-amber-100 ring-1 ring-inset ring-amber-600/50"
        >
          {segment.value}
        </span>
      );
    default:
      // Exhaustive over LogSegment — a new engine segment type fails to compile
      // here instead of silently rendering nothing.
      return assertNever(segment);
  }
}

// Memoized: the log panel re-renders on every search keystroke, filter toggle,
// and verbosity change. Entry objects are stable references (append-only log,
// preserved through the filter pipeline) and onInspectObjectSticky is a stable
// store action, so memo lets unchanged rows skip re-rendering on those panel updates.
export const LogEntry = memo(function LogEntry({
  entry,
  categoryLabel,
  showCategoryLabel = false,
  onInspectObjectSticky,
}: LogEntryProps) {
  const presentation = logPresentation(entry);
  const surfaceClass = toneClass(presentation.tone);
  const emphasisClass = importanceClass(presentation.importance);
  const seatOrder = useGameStore((s) => s.gameState?.seat_order);

  return (
    <div
      data-category={entry.category}
      data-importance={presentation.importance}
      data-tone={presentation.tone}
      className={`flex gap-2 border-b border-l-2 border-b-gray-800/70 px-2.5 py-2 text-sm leading-5 break-words ${surfaceClass} ${emphasisClass}`}
    >
      <span
        aria-hidden="true"
        className="mt-0.5 inline-flex h-5 w-5 shrink-0 items-center justify-center rounded bg-black/25 text-[10px] font-bold text-gray-300 ring-1 ring-inset ring-white/10"
      >
        {CATEGORY_GLYPHS[entry.category]}
      </span>
      <span className="min-w-0 flex-1">
        {showCategoryLabel && categoryLabel ? (
          <span className="mr-1.5 inline-flex rounded bg-black/25 px-1.5 py-px text-[9px] font-bold uppercase tracking-wider text-gray-400 ring-1 ring-inset ring-white/10">
            {categoryLabel}
          </span>
        ) : categoryLabel ? (
          <span className="sr-only">{`${categoryLabel}: `}</span>
        ) : null}
        {entry.segments.map((segment, index) =>
          renderSegment(segment, index, seatOrder, onInspectObjectSticky),
        )}
      </span>
    </div>
  );
});
