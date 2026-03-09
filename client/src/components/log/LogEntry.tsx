import type { GameEvent } from "../../adapter/types.ts";
import { formatEvent, classifyEventColor } from "../../viewmodel/logFormatting.ts";

const COLOR_MAP: Record<string, string> = {
  red: "text-red-400",
  blue: "text-blue-400",
  green: "text-green-400",
  gray: "text-gray-400",
};

interface LogEntryProps {
  event: GameEvent;
}

export function LogEntry({ event }: LogEntryProps) {
  const text = formatEvent(event);
  const color = classifyEventColor(event);
  const colorClass = COLOR_MAP[color] ?? "text-gray-400";

  return (
    <div className={`border-b border-gray-800 py-0.5 font-mono text-[10px] ${colorClass}`}>
      {text}
    </div>
  );
}
