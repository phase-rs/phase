import { useMemo } from "react";

import type { GameObject } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { partitionByType, groupByName } from "../../viewmodel/battlefieldProps.ts";
import { BattlefieldRow } from "./BattlefieldRow.tsx";

export function GameBoard() {
  const gameState = useGameStore((s) => s.gameState);

  const { opponent, player } = useMemo(() => {
    if (!gameState) return { opponent: null, player: null };

    const battlefieldObjects = gameState.battlefield
      .map((id) => gameState.objects[id])
      .filter(Boolean);

    const playerObjects = battlefieldObjects.filter(
      (obj) => obj.controller === 0,
    );
    const opponentObjects = battlefieldObjects.filter(
      (obj) => obj.controller === 1,
    );

    const partitionAndGroup = (objects: GameObject[]) => {
      const partition = partitionByType(objects);
      const objectMap = new Map(objects.map((o) => [o.id, o]));
      const resolveObjects = (ids: number[]) =>
        ids.map((id) => objectMap.get(id)).filter(Boolean) as GameObject[];

      return {
        creatures: groupByName(resolveObjects(partition.creatures)),
        lands: groupByName(resolveObjects(partition.lands)),
        other: groupByName(resolveObjects(partition.other)),
      };
    };

    return {
      player: partitionAndGroup(playerObjects),
      opponent: partitionAndGroup(opponentObjects),
    };
  }, [gameState]);

  if (!gameState) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <span className="text-gray-500">Waiting for game...</span>
      </div>
    );
  }

  return (
    <div className="flex flex-1 flex-col bg-gray-950">
      {/* Opponent's battlefield (other, creatures, lands from top) */}
      <div className="flex flex-col gap-1 border-b border-gray-800 py-1">
        {opponent && (
          <>
            <BattlefieldRow groups={opponent.other} rowType="other" />
            <BattlefieldRow groups={opponent.creatures} rowType="creatures" />
            <BattlefieldRow groups={opponent.lands} rowType="lands" />
          </>
        )}
      </div>

      {/* Middle spacer (stack/controls will go here in Plan 04) */}
      <div className="flex min-h-[40px] flex-1 items-center justify-center">
        <span className="text-xs text-gray-600">
          Turn {gameState.turn_number} &middot; {gameState.phase}
        </span>
      </div>

      {/* Player's battlefield (lands, creatures, other from bottom) */}
      <div className="flex flex-col gap-1 border-t border-gray-800 py-1">
        {player && (
          <>
            <BattlefieldRow groups={player.other} rowType="other" />
            <BattlefieldRow groups={player.creatures} rowType="creatures" />
            <BattlefieldRow groups={player.lands} rowType="lands" />
          </>
        )}
      </div>
    </div>
  );
}
