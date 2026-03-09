import { useMemo, useRef } from "react";

import type { GameObject, ManaColor } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import type { BoardBackground } from "../../stores/preferencesStore.ts";
import { partitionByType, groupByName } from "../../viewmodel/battlefieldProps.ts";
import { getDominantManaColor } from "../../viewmodel/dominantColor.ts";
import { BATTLEFIELD_MAP, getRandomBattlefield } from "./battlefields.ts";
import { BattlefieldRow } from "./BattlefieldRow.tsx";

function resolveBattlefieldImage(
  boardBackground: BoardBackground,
  dominantColor: ManaColor | null,
  autoRef: React.RefObject<string | null>,
): string | null {
  if (boardBackground === "none") return null;

  if (boardBackground === "auto-wubrg") {
    if (!dominantColor) return null;
    // Persist auto-selected battlefield for session to avoid flickering
    if (!autoRef.current) {
      autoRef.current = getRandomBattlefield(dominantColor).image;
    }
    return autoRef.current;
  }

  return BATTLEFIELD_MAP[boardBackground]?.image ?? null;
}

export function GameBoard() {
  const gameState = useGameStore((s) => s.gameState);
  const boardBackground = usePreferencesStore((s) => s.boardBackground);
  const autoImageRef = useRef<string | null>(null);

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

  const bgImage = resolveBattlefieldImage(boardBackground, dominantColor, autoImageRef);

  if (!gameState) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <span className="text-gray-500">Waiting for game...</span>
      </div>
    );
  }

  return (
    <div className="relative flex flex-1 flex-col">
      {/* Background image layer */}
      {bgImage ? (
        <div
          className="absolute inset-0 bg-cover bg-center"
          style={{ backgroundImage: `url(${bgImage})` }}
        >
          {/* Dark overlay so cards remain readable */}
          <div className="absolute inset-0 bg-black/40" />
        </div>
      ) : (
        <div className="absolute inset-0 bg-gray-950" />
      )}

      {/* Opponent's battlefield (mirrored: lands at top, creatures middle, other nearest center) */}
      <div className="relative flex flex-col gap-1 border-b border-white/10 bg-black/20 px-3 py-2">
        {opponent && (
          <>
            <BattlefieldRow groups={opponent.lands} rowType="lands" />
            <BattlefieldRow groups={opponent.creatures} rowType="creatures" />
            <BattlefieldRow groups={opponent.other} rowType="other" />
          </>
        )}
      </div>

      {/* Middle spacer */}
      <div className="relative flex min-h-[60px] flex-1 items-center justify-center border-y border-white/5" />

      {/* Player's battlefield (lands, creatures, other from bottom) */}
      <div className="relative flex flex-col gap-1 border-t border-white/10 px-3 py-2">
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
