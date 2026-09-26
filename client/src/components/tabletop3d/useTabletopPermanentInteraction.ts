import { useCallback, useMemo } from "react";

import type { GameAction, ObjectId } from "../../adapter/types.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import {
  useCanActForWaitingState,
  usePlayerId,
} from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import {
  collectObjectActions,
  isManaObjectAction,
  resolveSingleActionDispatch,
} from "../../viewmodel/cardActionChoice.ts";
import {
  boardChoiceMaxSelection,
  buildBoardChoiceAction,
  getBoardChoiceView,
  getWaitingForObjectChoiceIds,
  isBoardChoiceImmediate,
} from "../../viewmodel/gameStateView.ts";

export interface TabletopPermanentInteraction {
  hasProminentAction: boolean;
  isActionable: boolean;
  isAttacking: boolean;
  isBlocking: boolean;
  isHovered: boolean;
  isSelected: boolean;
  isValidTarget: boolean;
  onClick: () => void;
  onPointerEnter: () => void;
  onPointerLeave: () => void;
}

/**
 * Common battlefield interaction controller for a Three.js permanent. It
 * consumes the same engine-authored legal actions and waiting-state choices as
 * the DOM board; no rules legality is reconstructed in the scene.
 */
export function useTabletopPermanentInteraction(
  objectId: ObjectId,
): TabletopPermanentInteraction {
  const gameState = useGameStore((state) => state.gameState);
  const waitingFor = useGameStore((state) => state.waitingFor);
  const legalActionsByObject = useGameStore((state) => state.legalActionsByObject);
  const playerId = usePlayerId();
  const canActForWaitingState = useCanActForWaitingState();

  const selectedObjectId = useUiStore((state) => state.selectedObjectId);
  const selectedCardIds = useUiStore((state) => state.selectedCardIds);
  const selectedAttackers = useUiStore((state) => state.selectedAttackers);
  const blockerAssignments = useUiStore((state) => state.blockerAssignments);
  const combatMode = useUiStore((state) => state.combatMode);
  const combatClickHandler = useUiStore((state) => state.combatClickHandler);
  const isHovered = useUiStore((state) => state.hoveredObjectId === objectId);
  const selectObject = useUiStore((state) => state.selectObject);
  const hoverObject = useUiStore((state) => state.hoverObject);
  const inspectObject = useUiStore((state) => state.inspectObject);
  const toggleAttacker = useUiStore((state) => state.toggleAttacker);
  const toggleSelectedCard = useUiStore((state) => state.toggleSelectedCard);
  const setPendingAbilityChoice = useUiStore((state) => state.setPendingAbilityChoice);

  const object = gameState?.objects[objectId];
  const objectActions = useMemo(
    () => collectObjectActions(legalActionsByObject, objectId),
    [legalActionsByObject, objectId],
  );
  const hasProminentAction = objectActions.some(
    (action) => !isManaObjectAction(action, object),
  );
  const validTargetIds = useMemo(
    () => new Set(getWaitingForObjectChoiceIds(waitingFor)),
    [waitingFor],
  );
  const boardChoice = useMemo(() => {
    const choice = getBoardChoiceView(waitingFor, gameState?.objects);
    return canActForWaitingState && choice?.player === playerId ? choice : null;
  }, [canActForWaitingState, gameState?.objects, playerId, waitingFor]);

  const validAttacker =
    waitingFor?.type === "DeclareAttackers"
    && waitingFor.data.valid_attacker_ids.includes(objectId);
  const isValidTarget = validTargetIds.has(objectId);
  const isBoardChoice = boardChoice?.objectIds.includes(objectId) ?? false;
  const isSelectedForBoardChoice =
    isBoardChoice && selectedCardIds.includes(objectId);
  const undoableTap =
    (waitingFor?.type === "Priority"
      || waitingFor?.type === "ManaPayment"
      || waitingFor?.type === "UnlessPayment")
    && (gameState?.lands_tapped_for_mana?.[playerId]?.includes(objectId) ?? false);
  const isAttacking =
    selectedAttackers.includes(objectId)
    || (gameState?.combat?.attackers.some((attacker) => attacker.object_id === objectId) ?? false);
  const isBlocking = blockerAssignments.has(objectId);

  const onClick = useCallback(() => {
    if (!object) return;

    if (isBoardChoice && boardChoice) {
      if (isBoardChoiceImmediate(boardChoice)) {
        void dispatchAction(buildBoardChoiceAction(boardChoice, [objectId]));
        return;
      }
      const selectedForChoice = selectedCardIds.filter((id) =>
        boardChoice.objectIds.includes(id),
      );
      const maxSelection = boardChoiceMaxSelection(boardChoice);
      if (
        isSelectedForBoardChoice
        || maxSelection == null
        || selectedForChoice.length < maxSelection
      ) {
        toggleSelectedCard(objectId);
      }
      return;
    }

    if (combatMode === "attackers" && waitingFor?.type === "DeclareAttackers") {
      if (validAttacker) toggleAttacker(objectId);
      return;
    }

    if (
      combatMode === "blockers"
      && waitingFor?.type === "DeclareBlockers"
      && combatClickHandler
    ) {
      combatClickHandler(objectId);
      return;
    }

    if (
      waitingFor?.type === "EquipTarget"
      && waitingFor.data.valid_targets.includes(objectId)
    ) {
      void dispatchAction({
        type: "Equip",
        data: {
          equipment_id: waitingFor.data.equipment_id,
          target_id: objectId,
        },
      });
      return;
    }

    if (isValidTarget) {
      void dispatchAction({
        type: "ChooseTarget",
        data: { target: { Object: objectId } },
      });
      return;
    }

    if (objectActions.length > 0) {
      const automatic = resolveSingleActionDispatch(objectActions, object);
      if (automatic) {
        void dispatchAction(automatic);
      } else {
        setPendingAbilityChoice({
          objectId,
          actions: objectActions as GameAction[],
        });
      }
      return;
    }

    if (undoableTap) {
      void dispatchAction({
        type: "UntapLandForMana",
        data: { object_id: objectId },
      });
      return;
    }

    selectObject(selectedObjectId === objectId ? null : objectId);
  }, [
    boardChoice,
    combatClickHandler,
    combatMode,
    isBoardChoice,
    isSelectedForBoardChoice,
    isValidTarget,
    object,
    objectActions,
    objectId,
    selectObject,
    selectedCardIds,
    selectedObjectId,
    setPendingAbilityChoice,
    toggleAttacker,
    toggleSelectedCard,
    undoableTap,
    validAttacker,
    waitingFor,
  ]);

  const onPointerEnter = useCallback(() => {
    hoverObject(objectId);
    inspectObject(objectId);
  }, [hoverObject, inspectObject, objectId]);

  const onPointerLeave = useCallback(() => {
    hoverObject(null);
    inspectObject(null);
  }, [hoverObject, inspectObject]);

  return {
    // Mana sources remain clickable but do not receive a persistent cyan ring.
    // The glow is reserved for non-mana activations and explicit game choices.
    hasProminentAction,
    isActionable:
      isBoardChoice
      || validAttacker
      || isValidTarget
      || objectActions.length > 0
      || undoableTap,
    isAttacking,
    isBlocking,
    isHovered,
    isSelected: selectedObjectId === objectId || isSelectedForBoardChoice,
    isValidTarget,
    onClick,
    onPointerEnter,
    onPointerLeave,
  };
}
