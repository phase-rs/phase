import { useMemo } from "react";

import type { GameObject, ManaColor } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import type { BoardBackground } from "../../stores/preferencesStore.ts";
import { partitionByType, groupByName } from "../../viewmodel/battlefieldProps.ts";
import { getDominantManaColor } from "../../viewmodel/dominantColor.ts";
import { BattlefieldRow } from "./BattlefieldRow.tsx";

const COLOR_GRADIENTS: Record<ManaColor, string> = {
  White: "from-amber-900/50 via-gray-950 to-amber-900/50",
  Blue: "from-blue-900/60 via-gray-950 to-blue-900/60",
  Black: "from-purple-950/40 via-gray-900/60 to-purple-950/40",
  Red: "from-red-900/50 via-gray-950 to-red-900/50",
  Green: "from-emerald-900/50 via-gray-950 to-emerald-900/50",
};

const BG_TO_MANA_COLOR: Record<string, ManaColor> = {
  white: "White",
  blue: "Blue",
  black: "Black",
  red: "Red",
  green: "Green",
};

function getBoardGradient(
  boardBackground: BoardBackground,
  dominantColor: ManaColor | null,
): string {
  if (boardBackground === "none") return "bg-gray-950";

  if (boardBackground === "auto-wubrg") {
    if (!dominantColor) return "bg-gray-950";
    return `bg-gradient-to-r ${COLOR_GRADIENTS[dominantColor]}`;
  }

  const manaColor = BG_TO_MANA_COLOR[boardBackground];
  if (manaColor) {
    return `bg-gradient-to-r ${COLOR_GRADIENTS[manaColor]}`;
  }

  return "bg-gray-950";
}

export function GameBoard() {
  const gameState = useGameStore((s) => s.gameState);
  const boardBackground = usePreferencesStore((s) => s.boardBackground);

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

  const dominantColor = useMemo(() => {
    if (!gameState) return null;
    return getDominantManaColor(gameState.battlefield, gameState.objects, 0);
  }, [gameState]);

  const gradientClass = getBoardGradient(boardBackground, dominantColor);

  if (!gameState) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <span className="text-gray-500">Waiting for game...</span>
      </div>
    );
  }

  return (
    <div className={`flex flex-1 flex-col ${gradientClass}`}>
      {/* Opponent's battlefield (mirrored: lands at top, creatures middle, other nearest center) */}
      <div className="flex flex-col gap-1 border-b border-gray-800 bg-black/20 px-3 py-2">
        {opponent && (
          <>
            <BattlefieldRow groups={opponent.lands} rowType="lands" />
            <BattlefieldRow groups={opponent.creatures} rowType="creatures" />
            <BattlefieldRow groups={opponent.other} rowType="other" />
          </>
        )}
      </div>

      {/* Middle spacer */}
      <div className="flex min-h-[60px] flex-1 items-center justify-center border-y border-white/5 shadow-[inset_0_0_20px_rgba(255,255,255,0.03)]" />

      {/* Player's battlefield (lands, creatures, other from bottom) */}
      <div className="flex flex-col gap-1 border-t border-gray-800 px-3 py-2">
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
