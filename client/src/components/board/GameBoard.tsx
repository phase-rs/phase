import { useMemo } from "react";

import type { GameObject } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { partitionByType, groupByName } from "../../viewmodel/battlefieldProps.ts";
import { sortCreaturesForBlockers } from "../../viewmodel/blockerSorting.ts";
import { BattlefieldRow } from "./BattlefieldRow.tsx";

/**
 * CSS variable overrides for the land column.
 * By redefining --art-crop-w/h and --card-w/h on this container,
 * all card components inside render at the smaller land size.
 */
const LAND_SCALE = 0.65;

const LAND_COL_STYLE = {
  "--art-crop-w": `calc(var(--art-crop-base) * var(--card-size-scale) * ${LAND_SCALE})`,
  "--art-crop-h": `calc(var(--art-crop-base) * var(--card-size-scale) * ${LAND_SCALE} * 0.75)`,
  "--card-w": `calc(var(--card-base) * var(--card-size-scale) * ${LAND_SCALE + 0.2})`,
  "--card-h": `calc(var(--card-base) * var(--card-size-scale) * ${LAND_SCALE + 0.2} * 1.4)`,
  width: `calc(var(--art-crop-base) * var(--card-size-scale) * ${LAND_SCALE} * 2 + 1.5rem)`,
} as React.CSSProperties;

/** Symmetric padding so justify-center aligns creatures to viewport center */
const LAND_COL_WIDTH = `calc(var(--art-crop-base) * var(--card-size-scale) * ${LAND_SCALE} * 2 + 1.5rem)`;

export function GameBoard() {
  const gameState = useGameStore((s) => s.gameState);
  const canUndo = useGameStore((s) => s.stateHistory.length > 0);
  const undo = useGameStore((s) => s.undo);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);

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

  // Reorder player creatures so assigned blockers align with their target attackers
  const sortedPlayerCreatures = useMemo(
    () =>
      player && opponent
        ? sortCreaturesForBlockers(player.creatures, opponent.creatures, blockerAssignments)
        : player?.creatures ?? [],
    [player, opponent, blockerAssignments],
  );

  if (!gameState) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <span className="text-gray-500">Waiting for game...</span>
      </div>
    );
  }

  return (
    <div className="relative flex min-h-0 flex-1 flex-col overflow-visible">
      {/* Opponent's battlefield */}
      <div className="relative flex flex-1">
        {/* Opponent lands — absolutely positioned left column */}
        <div
          className="absolute left-0 top-0 bottom-0 z-10 overflow-y-auto px-1 py-2"
          style={LAND_COL_STYLE}
        >
          {opponent && (
            <BattlefieldRow groups={opponent.lands} rowType="lands" />
          )}
        </div>
        {/* Opponent creatures + other — symmetric padding keeps center at viewport center */}
        <div
          className="flex flex-1 flex-col justify-end gap-1 py-2"
          style={{ paddingLeft: LAND_COL_WIDTH, paddingRight: LAND_COL_WIDTH }}
        >
          {opponent && (
            <>
              <BattlefieldRow groups={opponent.other} rowType="other" />
              <BattlefieldRow groups={opponent.creatures} rowType="creatures" />
            </>
          )}
        </div>
      </div>

      {/* Minimal center gap */}
      <div className="h-1" />

      {/* Player's battlefield */}
      <div className="relative flex flex-1">
        {/* Player lands — absolutely positioned left column */}
        <div
          className="absolute left-0 top-0 bottom-0 z-10 flex flex-col overflow-y-auto px-1 py-2"
          style={LAND_COL_STYLE}
        >
          {player && (
            <BattlefieldRow groups={player.lands} rowType="lands" />
          )}
          {canUndo && (
            <button
              onClick={undo}
              className="mt-auto mx-auto flex items-center gap-1 rounded-md bg-gray-800/80 px-2.5 py-1 text-[11px] font-medium text-gray-400 transition-colors hover:bg-gray-700/80 hover:text-gray-200"
            >
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="h-3 w-3">
                <path fillRule="evenodd" d="M14 8a6 6 0 1 1-12 0 6 6 0 0 1 12 0ZM7.72 4.22a.75.75 0 0 0-1.06 0L4.97 5.91a.75.75 0 0 0 0 1.06l1.69 1.69a.75.75 0 1 0 1.06-1.06l-.47-.47h1.63a1.25 1.25 0 0 1 0 2.5H7.5a.75.75 0 0 0 0 1.5h1.38a2.75 2.75 0 0 0 0-5.5H7.25l.47-.47a.75.75 0 0 0 0-1.06Z" clipRule="evenodd" />
              </svg>
              Undo
            </button>
          )}
        </div>
        {/* Player creatures + other — symmetric padding keeps center at viewport center */}
        <div
          className="flex flex-1 flex-col gap-1 pt-2 pb-4"
          style={{ paddingLeft: LAND_COL_WIDTH, paddingRight: LAND_COL_WIDTH }}
        >
          {player && (
            <>
              <BattlefieldRow groups={sortedPlayerCreatures} rowType="creatures" />
              <BattlefieldRow groups={player.other} rowType="other" />
            </>
          )}
        </div>
      </div>
    </div>
  );
}
