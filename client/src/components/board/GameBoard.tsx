import { useMemo } from "react";

import type { PlayerId } from "../../adapter/types.ts";
import { isMultiplayerMode, useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useCanActForWaitingState, usePerspectivePlayerId, usePlayerId } from "../../hooks/usePlayerId.ts";
import { sortCreaturesForBlockers } from "../../viewmodel/blockerSorting.ts";
import { isManaObjectAction } from "../../viewmodel/cardActionChoice.ts";
import {
  buildPlayerBattlefieldView,
  getWaitingForObjectChoiceIds,
  getOpponentIds,
} from "../../viewmodel/gameStateView.ts";
import { BoardInteractionContext } from "./BoardInteractionContext.tsx";
import { CombatLine } from "./CombatLine.tsx";
import { PlayerArea } from "./PlayerArea.tsx";

export function GameBoard() {
  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  // Undo is a single-player affordance only — multiplayer games have
  // authoritative shared state and can't safely rewind one client.
  const canUndo = useGameStore(
    (s) => s.stateHistory.length > 0 && !isMultiplayerMode(s.gameMode),
  );
  const undo = useGameStore((s) => s.undo);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const localPlayerId = usePlayerId();
  const myId = usePerspectivePlayerId();
  const canActForWaitingState = useCanActForWaitingState();

  // Track which opponent is focused (expanded) in multiplayer
  const focusedOpponent = useUiStore((s) => s.focusedOpponent) as PlayerId | null;

  const opponents = useMemo(() => {
    return getOpponentIds(gameState, myId);
  }, [gameState, myId]);

  const focusedId = focusedOpponent ?? (opponents.length === 1 ? opponents[0] : null);
  const playerBattlefieldView = useMemo(
    () => buildPlayerBattlefieldView(gameState, myId),
    [gameState, myId],
  );
  const focusedBattlefieldView = useMemo(
    () => (focusedId == null ? null : buildPlayerBattlefieldView(gameState, focusedId)),
    [gameState, focusedId],
  );

  const sortedPlayerCreatures = useMemo(() => {
    if (!focusedBattlefieldView) return undefined;
    return sortCreaturesForBlockers(
      playerBattlefieldView.creatures,
      focusedBattlefieldView.creatures,
      blockerAssignments,
    );
  }, [playerBattlefieldView, focusedBattlefieldView, blockerAssignments]);

  const boardInteractionState = useMemo(() => {
    const validTargetObjectIds = new Set<number>();
    const validAttackerIds = new Set<number>();
    const activatableObjectIds = new Set<number>();
    const manaTappableObjectIds = new Set<number>();
    const selectableManaCostCreatureIds = new Set<number>();
    const undoableTapObjectIds = new Set<number>();
    const committedAttackerIds = new Set<number>();
    const incomingAttackerCounts = new Map<number, number>();

    if (gameState?.combat?.attackers) {
      for (const attacker of gameState.combat.attackers) {
        committedAttackerIds.add(attacker.object_id);
        // Accumulate incoming-attack counts for permanent targets (Planeswalker,
        // Battle). Player targets are handled via HUD `underAttack` treatment.
        const t = attacker.attack_target;
        if (t.type === "Planeswalker" || t.type === "Battle") {
          incomingAttackerCounts.set(t.data, (incomingAttackerCounts.get(t.data) ?? 0) + 1);
        }
      }
    }

    if (gameState?.lands_tapped_for_mana?.[localPlayerId]) {
      for (const objectId of gameState.lands_tapped_for_mana[localPlayerId]) {
        undoableTapObjectIds.add(objectId);
      }
    }

    if (waitingFor?.type === "DeclareAttackers") {
      for (const objectId of waitingFor.data.valid_attacker_ids ?? []) {
        validAttackerIds.add(objectId);
      }
    }

    for (const objectId of getWaitingForObjectChoiceIds(waitingFor)) {
      validTargetObjectIds.add(objectId);
    }

    if (waitingFor?.type === "TapCreaturesForManaAbility" || waitingFor?.type === "TapCreaturesForSpellCost") {
      for (const objectId of waitingFor.data.creatures) {
        selectableManaCostCreatureIds.add(objectId);
      }
    }

    if (!gameState?.objects) {
      return {
        activatableObjectIds,
        committedAttackerIds,
        incomingAttackerCounts,
        manaTappableObjectIds,
        selectableManaCostCreatureIds,
        undoableTapObjectIds,
        validAttackerIds,
        validTargetObjectIds,
      };
    }

    const playerCanAct =
      waitingFor != null
      && (
        (waitingFor.type === "Priority" && canActForWaitingState)
        || (waitingFor.type === "ManaPayment" && canActForWaitingState)
        || (waitingFor.type === "UnlessPayment" && canActForWaitingState)
      );

    if (waitingFor?.type === "Priority" && canActForWaitingState) {
      // The engine owns the "which permanent does this action act on" mapping
      // via GameAction::source_object(), exposed as `legalActionsByObject`.
      // The cyan activatable ring surfaces battlefield permanents with at
      // least one non-mana action; mana abilities are handled by the separate
      // mana-tappable ring below. This iteration is variant-agnostic — adding
      // a future keyword activation requires zero frontend changes.
      for (const [idStr, actions] of Object.entries(legalActionsByObject)) {
        const objectId = Number(idStr);
        const object = gameState.objects[objectId];
        if (!object) continue;
        const hasNonManaAction = actions.some((action) => !isManaObjectAction(action, object));
        if (hasNonManaAction) {
          activatableObjectIds.add(objectId);
        }
      }
    }

    if (playerCanAct) {
      for (const [idStr, actions] of Object.entries(legalActionsByObject)) {
        const objectId = Number(idStr);
        const object = gameState.objects[objectId];
        if (!object) continue;
        if (actions.some((action) => isManaObjectAction(action, object))) {
          manaTappableObjectIds.add(objectId);
        }
      }
    }

    return {
      activatableObjectIds,
      committedAttackerIds,
      incomingAttackerCounts,
      manaTappableObjectIds,
      selectableManaCostCreatureIds,
      undoableTapObjectIds,
      validAttackerIds,
      validTargetObjectIds,
    };
  }, [canActForWaitingState, gameState, legalActionsByObject, localPlayerId, waitingFor]);

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
    <BoardInteractionContext.Provider value={boardInteractionState}>
      <div className="relative flex min-h-0 flex-1 flex-col">
        {/* Opponent area */}
        {is1v1 ? (
          <PlayerArea
            battlefieldView={focusedBattlefieldView ?? undefined}
            playerId={opponents[0]}
            mode="focused"
          />
        ) : (
          <div className="flex min-h-0 flex-1 flex-col">
            {focusedOpponent != null && opponents.includes(focusedOpponent) ? (
              <PlayerArea
                battlefieldView={focusedBattlefieldView ?? undefined}
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

        <CombatLine />

        <PlayerArea
          battlefieldView={playerBattlefieldView}
          playerId={myId}
          mode="full"
          landColumnExtra={undoButton}
          creatureOverride={sortedPlayerCreatures}
        />
      </div>
    </BoardInteractionContext.Provider>
  );
}
