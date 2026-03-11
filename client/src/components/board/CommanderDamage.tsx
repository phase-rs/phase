import { useMemo } from "react";

import type { PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";

interface CommanderDamageProps {
  playerId: PlayerId;
}

/** Threshold at which commander damage eliminates a player (MTG rule). */
const COMMANDER_DAMAGE_LETHAL = 21;

export function CommanderDamage({ playerId }: CommanderDamageProps) {
  const gameState = useGameStore((s) => s.gameState);

  const damageEntries = useMemo(() => {
    if (!gameState?.commander_damage) return [];

    return gameState.commander_damage.filter(
      (entry) => entry.player === playerId && entry.damage > 0,
    );
  }, [gameState, playerId]);

  if (damageEntries.length === 0) return null;

  return (
    <div
      className="flex flex-wrap gap-1"
      data-testid={`commander-damage-${playerId}`}
    >
      {damageEntries.map((entry) => {
        const obj = gameState?.objects[entry.commander];
        const name = obj?.name ?? `#${entry.commander}`;
        const isLethal = entry.damage >= COMMANDER_DAMAGE_LETHAL;
        const isWarning = entry.damage >= COMMANDER_DAMAGE_LETHAL - 5;

        return (
          <div
            key={`${entry.player}-${entry.commander}`}
            className={`flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-medium ${
              isLethal
                ? "bg-red-900/80 text-red-200"
                : isWarning
                  ? "bg-yellow-900/60 text-yellow-200"
                  : "bg-gray-800/80 text-gray-300"
            }`}
            title={`Commander damage from ${name}: ${entry.damage}/${COMMANDER_DAMAGE_LETHAL}`}
          >
            <span className="max-w-[60px] truncate">{name}</span>
            <span className="tabular-nums font-bold">{entry.damage}</span>
          </div>
        );
      })}
    </div>
  );
}
