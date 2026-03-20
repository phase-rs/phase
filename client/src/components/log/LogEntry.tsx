import type { GameLogEntry, LogSegment } from "../../adapter/types.ts";
import { categoryColorClass } from "../../viewmodel/logFormatting.ts";

const PLAYER_COLORS = [
  "text-cyan-300",    // Player 1 (you)
  "text-orange-300",  // Player 2 (opponent)
  "text-emerald-300", // Player 3
  "text-pink-300",    // Player 4
];

interface LogEntryProps {
  entry: GameLogEntry;
}

function playerColor(playerId: number): string {
  return PLAYER_COLORS[playerId % PLAYER_COLORS.length];
}

function renderSegment(segment: LogSegment, index: number) {
  switch (segment.type) {
    case "Text":
      return <span key={index}>{segment.value}</span>;
    case "CardName":
      return (
        <span key={index} className="font-semibold text-yellow-300">
          {segment.value.name}
        </span>
      );
    case "PlayerName":
      return (
        <span key={index} className={`font-semibold ${playerColor(segment.value.player_id)}`}>
          {segment.value.name}
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

export function LogEntry({ entry }: LogEntryProps) {
  const colorClass = categoryColorClass(entry);

  return (
    <div className={`border-b border-gray-800 py-0.5 font-mono text-[10px] ${colorClass}`}>
      {entry.segments.map(renderSegment)}
    </div>
  );
}
