import { useMemo } from "react";

import type { GameObject } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { BattlefieldRow } from "./BattlefieldRow.tsx";

function partitionByType(objects: GameObject[]) {
  const creatures: number[] = [];
  const lands: number[] = [];
  const other: number[] = [];

  for (const obj of objects) {
    if (obj.card_types.core_types.includes("Land")) {
      lands.push(obj.id);
    } else if (obj.card_types.core_types.includes("Creature")) {
      creatures.push(obj.id);
    } else {
      other.push(obj.id);
    }
  }

  return { creatures, lands, other };
}

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

    return {
      player: partitionByType(playerObjects),
      opponent: partitionByType(opponentObjects),
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
            <BattlefieldRow objectIds={opponent.other} rowType="other" />
            <BattlefieldRow objectIds={opponent.creatures} rowType="creatures" />
            <BattlefieldRow objectIds={opponent.lands} rowType="lands" />
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
            <BattlefieldRow objectIds={player.other} rowType="other" />
            <BattlefieldRow objectIds={player.creatures} rowType="creatures" />
            <BattlefieldRow objectIds={player.lands} rowType="lands" />
          </>
        )}
      </div>
    </div>
  );
}
