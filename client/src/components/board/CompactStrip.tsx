import { useMemo } from "react";

import type { PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { partitionByType } from "../../viewmodel/battlefieldProps.ts";

interface CompactStripProps {
  playerId: PlayerId;
  onClick?: () => void;
  isActive?: boolean;
}

export function CompactStrip({ playerId, onClick, isActive }: CompactStripProps) {
  const gameState = useGameStore((s) => s.gameState);
  const isTheirTurn = gameState?.active_player === playerId;

  const { player, counts } = useMemo(() => {
    if (!gameState) return { player: null, counts: { creatures: 0, lands: 0, other: 0 } };

    const p = gameState.players[playerId];
    const battlefieldObjects = gameState.battlefield
      .map((id) => gameState.objects[id])
      .filter(Boolean)
      .filter((obj) => obj.controller === playerId);

    const partition = partitionByType(battlefieldObjects);

    return {
      player: p,
      counts: {
        creatures: partition.creatures.length,
        lands: partition.lands.length,
        other: partition.support.length + partition.planeswalkers.length + partition.other.length,
      },
    };
  }, [gameState, playerId]);

  if (!player) return null;

  const isEliminated = player.is_eliminated ?? false;
  const handCount = player.hand.length;
  const lifeColor =
    player.life >= 10
      ? "text-green-400"
      : player.life >= 5
        ? "text-yellow-400"
        : "text-red-400";

  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex items-center gap-3 rounded-lg border-2 px-3 py-2 shadow-md transition-all duration-300 hover:border-gray-400 hover:bg-gray-800/80 ${isTheirTurn ? "border-red-400 bg-black/60 ring-2 ring-red-400/40 shadow-[0_0_12px_rgba(248,113,113,0.4)]" : isActive ? "border-amber-400 bg-gray-800/90 ring-2 ring-amber-400/40 shadow-amber-500/20" : "border-gray-500 bg-gray-900/80 shadow-black/30"} ${isEliminated ? "opacity-40 grayscale" : ""}`}
      data-testid={`compact-strip-${playerId}`}
    >
      {/* Player name and life */}
      <div className="flex flex-col items-start">
        <div className="flex items-center gap-1">
          {isTheirTurn && <span className="h-1.5 w-1.5 rounded-full bg-red-400 animate-pulse" />}
          <span className={`text-xs ${isTheirTurn ? "text-red-300 font-semibold" : "text-gray-400"}`}>Opp {playerId + 1}</span>
        </div>
        <span className={`text-lg font-bold tabular-nums ${lifeColor}`}>
          {player.life}
        </span>
      </div>

      {/* Hand count */}
      <div className="flex flex-col items-center" title="Cards in hand">
        <span className="text-[10px] text-gray-500">Hand</span>
        <span className="text-sm font-medium text-gray-300">{handCount}</span>
      </div>

      {/* Permanent counts */}
      {counts.creatures > 0 && (
        <PermanentCount label="Crt" count={counts.creatures} color="text-red-400" />
      )}
      {counts.lands > 0 && (
        <PermanentCount label="Lnd" count={counts.lands} color="text-green-400" />
      )}
      {counts.other > 0 && (
        <PermanentCount label="Oth" count={counts.other} color="text-blue-400" />
      )}

      {/* Eliminated badge */}
      {isEliminated && (
        <span className="ml-1 rounded bg-red-900/60 px-1.5 py-0.5 text-[10px] font-bold text-red-300">
          OUT
        </span>
      )}
    </button>
  );
}

function PermanentCount({ label, count, color }: { label: string; count: number; color: string }) {
  return (
    <div className="flex flex-col items-center">
      <span className="text-[10px] text-gray-500">{label}</span>
      <span className={`text-sm font-medium tabular-nums ${color}`}>{count}</span>
    </div>
  );
}
