import { useCallback, useEffect, useState } from "react";

import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type { ObjectId } from "../../adapter/types.ts";
import { AttackerControls } from "./AttackerControls.tsx";
import { BlockerControls } from "./BlockerControls.tsx";
import { BlockerArrow } from "./BlockerArrow.tsx";

interface CombatOverlayProps {
  mode: "attackers" | "blockers";
}

export function CombatOverlay({ mode }: CombatOverlayProps) {
  const dispatch = useGameDispatch();
  const setCombatMode = useUiStore((s) => s.setCombatMode);
  const clearCombatSelection = useUiStore((s) => s.clearCombatSelection);
  const selectedAttackers = useUiStore((s) => s.selectedAttackers);
  const selectAllAttackers = useUiStore((s) => s.selectAllAttackers);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const assignBlocker = useUiStore((s) => s.assignBlocker);
  const setCombatClickHandler = useUiStore((s) => s.setCombatClickHandler);

  const gameState = useGameStore((s) => s.gameState);

  // Blocker mode: track which blocker is pending assignment
  const [pendingBlocker, setPendingBlocker] = useState<ObjectId | null>(null);

  useEffect(() => {
    setCombatMode(mode);
    return () => {
      clearCombatSelection();
    };
  }, [mode, setCombatMode, clearCombatSelection]);

  // Compute valid attacker IDs (player 0's untapped creatures on battlefield)
  const validAttackerIds = (() => {
    if (!gameState) return [];
    const ids: ObjectId[] = [];
    for (const idStr of gameState.battlefield) {
      const obj = gameState.objects[idStr];
      if (
        obj &&
        obj.controller === 0 &&
        obj.card_types.core_types.includes("Creature") &&
        !obj.tapped
      ) {
        ids.push(obj.id);
      }
    }
    return ids;
  })();

  // Register blocker click handler
  const handleBlockerClick = useCallback(
    (objectId: ObjectId) => {
      if (!gameState) return;

      if (pendingBlocker === null) {
        // First click: select a blocker (player 0's creature on battlefield)
        const obj = gameState.objects[objectId];
        if (
          obj &&
          obj.controller === 0 &&
          obj.card_types.core_types.includes("Creature")
        ) {
          setPendingBlocker(objectId);
        }
      } else {
        // Second click: assign to an attacker (opponent's attacking creature)
        const attackers =
          gameState.combat?.attackers.map((a) => a.object_id) ?? [];
        if (attackers.includes(objectId)) {
          assignBlocker(pendingBlocker, objectId);
          setPendingBlocker(null);
        }
      }
    },
    [pendingBlocker, gameState, assignBlocker],
  );

  useEffect(() => {
    if (mode === "blockers") {
      setCombatClickHandler(handleBlockerClick);
    }
    return () => {
      if (mode === "blockers") {
        setCombatClickHandler(null);
      }
    };
  }, [mode, handleBlockerClick, setCombatClickHandler]);

  // Attacker handlers
  const handleAttackAll = useCallback(() => {
    selectAllAttackers(validAttackerIds);
  }, [selectAllAttackers, validAttackerIds]);

  const handleSkip = useCallback(() => {
    dispatch({ type: "DeclareAttackers", data: { attacker_ids: [] } });
    clearCombatSelection();
  }, [dispatch, clearCombatSelection]);

  const handleConfirmAttackers = useCallback(() => {
    dispatch({
      type: "DeclareAttackers",
      data: { attacker_ids: selectedAttackers },
    });
    clearCombatSelection();
  }, [dispatch, selectedAttackers, clearCombatSelection]);

  // Blocker handler
  const handleConfirmBlockers = useCallback(() => {
    dispatch({
      type: "DeclareBlockers",
      data: {
        assignments: Array.from(blockerAssignments.entries()),
      },
    });
    clearCombatSelection();
  }, [dispatch, blockerAssignments, clearCombatSelection]);

  if (mode === "attackers") {
    return (
      <AttackerControls
        onAttackAll={handleAttackAll}
        onSkip={handleSkip}
        onConfirm={handleConfirmAttackers}
        attackerCount={selectedAttackers.length}
      />
    );
  }

  // Blockers mode
  const entries = Array.from(blockerAssignments.entries());

  return (
    <>
      {entries.map(([blockerId, attackerId]) => (
        <BlockerArrow
          key={blockerId}
          blockerId={blockerId}
          attackerId={attackerId}
        />
      ))}
      <BlockerControls
        onConfirm={handleConfirmBlockers}
        assignmentCount={blockerAssignments.size}
      />
      {pendingBlocker !== null && (
        <div className="fixed inset-x-0 top-4 z-30 flex justify-center">
          <div className="rounded-lg bg-blue-900/80 px-4 py-2 text-sm font-medium text-blue-200 shadow-lg">
            Click an attacker to assign blocker
          </div>
        </div>
      )}
    </>
  );
}
