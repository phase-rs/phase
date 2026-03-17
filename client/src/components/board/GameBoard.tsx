import { useMemo } from "react";

import type { GameObject, PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { partitionByType, groupByName } from "../../viewmodel/battlefieldProps.ts";
import { sortCreaturesForBlockers } from "../../viewmodel/blockerSorting.ts";
import { PlayerArea } from "./PlayerArea.tsx";

export function GameBoard() {
  const gameState = useGameStore((s) => s.gameState);
  const canUndo = useGameStore((s) => s.stateHistory.length > 0);
  const undo = useGameStore((s) => s.undo);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const myId = usePlayerId();

  // Track which opponent is focused (expanded) in multiplayer
  const focusedOpponent = useUiStore((s) => s.focusedOpponent) as PlayerId | null;

  // Compute live opponents from seat order
  const opponents = useMemo(() => {
    if (!gameState) return [];
    const seatOrder = gameState.seat_order ?? gameState.players.map((p) => p.id);
    const eliminated = gameState.eliminated_players ?? [];
    return seatOrder.filter((id) => id !== myId && !eliminated.includes(id));
  }, [gameState, myId]);

  // For blocker sorting: compute grouped creatures for the focused/single opponent
  const focusedId = focusedOpponent ?? (opponents.length === 1 ? opponents[0] : null);

  const sortedPlayerCreatures = useMemo(() => {
    if (!gameState || focusedId == null) return undefined;

    const battlefieldObjects = gameState.battlefield
      .map((id) => gameState.objects[id])
      .filter(Boolean);

    const partitionAndGroup = (objects: GameObject[]) => {
      const partition = partitionByType(objects);
      const objectMap = new Map(objects.map((o) => [o.id, o]));
      const resolveObjects = (ids: number[]) =>
        ids.map((id) => objectMap.get(id)).filter(Boolean) as GameObject[];
      return {
        creatures: groupByName(resolveObjects(partition.creatures)),
      };
    };

    const playerObjects = battlefieldObjects.filter((obj) => obj.controller === myId);
    const opponentObjects = battlefieldObjects.filter((obj) => obj.controller === focusedId);

    const playerGroups = partitionAndGroup(playerObjects);
    const opponentGroups = partitionAndGroup(opponentObjects);

    return sortCreaturesForBlockers(playerGroups.creatures, opponentGroups.creatures, blockerAssignments);
  }, [gameState, myId, focusedId, blockerAssignments]);

  if (!gameState) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <span className="text-gray-500">Waiting for game...</span>
      </div>
    );
  }

  const is1v1 = opponents.length === 1;

  // Undo button for the player's land column
  const undoButton = canUndo ? (
    <button
      onClick={undo}
      className="mt-auto mx-auto flex items-center gap-1 rounded-md bg-gray-800/80 px-2.5 py-1 text-[11px] font-medium text-gray-400 transition-colors hover:bg-gray-700/80 hover:text-gray-200"
    >
      <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="h-3 w-3">
        <path fillRule="evenodd" d="M14 8a6 6 0 1 1-12 0 6 6 0 0 1 12 0ZM7.72 4.22a.75.75 0 0 0-1.06 0L4.97 5.91a.75.75 0 0 0 0 1.06l1.69 1.69a.75.75 0 1 0 1.06-1.06l-.47-.47h1.63a1.25 1.25 0 0 1 0 2.5H7.5a.75.75 0 0 0 0 1.5h1.38a2.75 2.75 0 0 0 0-5.5H7.25l.47-.47a.75.75 0 0 0 0-1.06Z" clipRule="evenodd" />
      </svg>
      Undo
    </button>
  ) : null;

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      {/* Opponent area */}
      {is1v1 ? (
        // 1v1: single opponent in focused mode (identical to original layout)
        <PlayerArea
          playerId={opponents[0]}
          mode="focused"
        />
      ) : (
        // Multiplayer: focused opponent battlefield (selection via OpponentHud tabs)
        <div className="flex min-h-0 flex-1 flex-col">
          {focusedOpponent != null && opponents.includes(focusedOpponent) ? (
            <PlayerArea
              playerId={focusedOpponent}
              mode="focused"
            />
          ) : (
            <div className="flex flex-1 items-center justify-center">
              <span className="text-xs text-gray-600">Click an opponent to view their board</span>
            </div>
          )}
        </div>
      )}

      {/* Minimal center gap */}
      <div className="h-1 shrink-0" />

      {/* Player's battlefield */}
      <PlayerArea
        playerId={myId}
        mode="full"
        landColumnExtra={undoButton}
        creatureOverride={sortedPlayerCreatures}
      />
    </div>
  );
}
