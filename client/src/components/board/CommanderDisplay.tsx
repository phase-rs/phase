import { useMemo } from "react";

import type { PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";

interface CommanderDisplayProps {
  playerId: PlayerId;
  compact?: boolean;
}

export function CommanderDisplay({ playerId, compact = false }: CommanderDisplayProps) {
  const gameState = useGameStore((s) => s.gameState);

  const commander = useMemo(() => {
    if (!gameState) return null;

    // Find commander objects owned by this player
    const allObjects = Object.values(gameState.objects);
    return allObjects.find(
      (obj) => obj.owner === playerId && obj.card_types.supertypes.includes("Legendary"),
    ) ?? null;
  }, [gameState, playerId]);

  const castCount = useMemo(() => {
    if (!gameState || !commander) return 0;
    // Commander cast count from state (tracked in engine as commander_cast_count)
    // Not yet serialized to frontend -- show 0 for now
    return 0;
  }, [gameState, commander]);

  if (!commander) return null;

  const sizeClass = compact ? "w-8 h-8 text-[9px]" : "w-12 h-12 text-xs";

  return (
    <div
      className={`flex items-center gap-1.5 rounded-md bg-gray-800/90 p-1 ${compact ? "px-1" : "px-2"}`}
      data-testid={`commander-display-${playerId}`}
      title={commander.name}
    >
      <div
        className={`${sizeClass} flex items-center justify-center rounded ring-2 ring-amber-500/70 bg-gray-700 font-bold text-amber-200 shadow-[0_0_6px_rgba(245,158,11,0.3)]`}
      >
        Cmd
      </div>
      <div className="flex flex-col">
        <span className={`font-medium text-gray-200 ${compact ? "text-[10px]" : "text-xs"} max-w-[80px] truncate`}>
          {commander.name}
        </span>
        {castCount > 0 && (
          <span className="text-[9px] text-amber-400">
            Tax: +{castCount * 2}
          </span>
        )}
      </div>
    </div>
  );
}
