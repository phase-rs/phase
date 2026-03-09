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
    <div className="relative flex min-h-0 flex-1 flex-col overflow-auto">
      {/* Opponent's battlefield: other (far) → lands → creatures (near center) */}
      <div className="relative flex flex-1 flex-col justify-end gap-1 px-3 py-2">
        {opponent && (
          <>
            <BattlefieldRow groups={opponent.other} rowType="other" />
            <BattlefieldRow groups={opponent.lands} rowType="lands" />
            <BattlefieldRow groups={opponent.creatures} rowType="creatures" />
          </>
        )}
      </div>

      {/* Minimal center gap */}
      <div className="h-1" />

      {/* Player's battlefield: creatures (near center) → lands → other (far) */}
      <div className="relative flex flex-1 flex-col gap-1 px-3 py-2">
        {player && (
          <>
            <BattlefieldRow groups={player.creatures} rowType="creatures" />
            <BattlefieldRow groups={player.lands} rowType="lands" />
            <BattlefieldRow groups={player.other} rowType="other" />
          </>
        )}
      </div>
    </div>
  );
}
