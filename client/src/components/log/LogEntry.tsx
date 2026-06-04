import type { GameLogEntry, LogSegment, ObjectId, PlayerId } from "../../adapter/types.ts";
import { getSeatColor } from "../../hooks/useSeatColor.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getPlayerDisplayName } from "../../stores/multiplayerStore.ts";
import { categoryColorClass } from "../../viewmodel/logFormatting.ts";

interface LogEntryProps {
  entry: GameLogEntry;
  onInspectObject?: (objectId: ObjectId) => void;
}

function renderSegment(
  segment: LogSegment,
  index: number,
  seatOrder: PlayerId[] | undefined,
  onInspectObject?: (objectId: ObjectId) => void,
) {
  switch (segment.type) {
    case "Text":
      return <span key={index}>{segment.value}</span>;
    case "CardName":
      return onInspectObject ? (
        <button
          key={index}
          type="button"
          onClick={() => onInspectObject(segment.value.object_id)}
          className="font-semibold text-yellow-300 underline decoration-yellow-500/40 underline-offset-2 transition hover:text-yellow-200"
        >
          {segment.value.name}
        </button>
      ) : (
        <span key={index} className="font-semibold text-yellow-300">
          {segment.value.name}
        </span>
      );
    case "PlayerName":
      return (
        <span
          key={index}
          className="font-semibold"
          style={{ color: getSeatColor(segment.value.player_id, seatOrder) }}
        >
          {getPlayerDisplayName(segment.value.player_id)}
        </span>
      );
    case "Number":
      return (
        <span key={index} className="font-bold text-white">
          {segment.value}
        </span>
      );
    case "Zone":
      return (
        <span key={index} className="italic">
          {segment.value}
        </span>
      );
    case "Keyword":
      return (
        <span key={index} className="text-purple-300">
          {segment.value}
        </span>
      );
    case "Mana":
      return (
        <span key={index} className="text-amber-200">
          {segment.value}
        </span>
      );
  }
}

export function LogEntry({ entry, onInspectObject }: LogEntryProps) {
  const colorClass = categoryColorClass(entry);
  const seatOrder = useGameStore((s) => s.gameState?.seat_order);

  return (
    <div className={`border-b border-gray-800 py-0.5 font-mono text-[10px] ${colorClass}`}>
      {entry.segments.map((segment, index) =>
        renderSegment(segment, index, seatOrder, onInspectObject),
      )}
    </div>
  );
}
