import { useCallback, useEffect, useMemo, useState } from "react";

import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import type { AttackTarget, ObjectId } from "../../adapter/types.ts";
import { buildAttacks, hasMultipleAttackTargets, getValidAttackTargets } from "../../utils/combat.ts";
import { AttackerControls } from "./AttackerControls.tsx";
import { BlockerControls } from "./BlockerControls.tsx";
import { BlockerArrow } from "./BlockerArrow.tsx";
import { AttackTargetPicker } from "../controls/AttackTargetPicker.tsx";

interface CombatOverlayProps {
  mode: "attackers" | "blockers";
}

export function CombatOverlay({ mode }: CombatOverlayProps) {
  const dispatch = useGameDispatch();
  const playerId = usePlayerId();
  const gameState = useGameStore((s) => s.gameState);
  const setCombatMode = useUiStore((s) => s.setCombatMode);
  const clearCombatSelection = useUiStore((s) => s.clearCombatSelection);
  const selectedAttackers = useUiStore((s) => s.selectedAttackers);
  const selectAllAttackers = useUiStore((s) => s.selectAllAttackers);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const assignBlocker = useUiStore((s) => s.assignBlocker);
  const setCombatClickHandler = useUiStore((s) => s.setCombatClickHandler);

  const waitingFor = useGameStore((s) => s.waitingFor);
  const combatAttackers = useGameStore(
    (s) => s.gameState?.combat?.attackers,
  );
  const combatAttackerIds = useMemo(
    () => combatAttackers?.map((a) => a.object_id) ?? [],
    [combatAttackers],
  );

  // Blocker mode: track which blocker is pending assignment
  const [pendingBlocker, setPendingBlocker] = useState<ObjectId | null>(null);

  // Attack target picker for multiplayer
  const [showTargetPicker, setShowTargetPicker] = useState(false);
  const isMultiTarget = hasMultipleAttackTargets(gameState);
  const validAttackTargets = getValidAttackTargets(gameState);

  useEffect(() => {
    setCombatMode(mode);
    return () => {
      clearCombatSelection();
    };
  }, [mode, setCombatMode, clearCombatSelection]);

  // Valid attacker IDs from engine
  const validAttackerIds = useMemo(
    () =>
      waitingFor?.type === "DeclareAttackers"
        ? (waitingFor.data.valid_attacker_ids ?? [])
        : [],
    [waitingFor],
  );

  // Valid blocker IDs from engine
  const validBlockerIds = useMemo(
    () =>
      waitingFor?.type === "DeclareBlockers"
        ? (waitingFor.data.valid_blocker_ids ?? [])
        : [],
    [waitingFor],
  );

  // Register blocker click handler
  const handleBlockerClick = useCallback(
    (objectId: ObjectId) => {
      if (pendingBlocker === null) {
        // First click: select a valid blocker (engine-validated)
        if (validBlockerIds.includes(objectId)) {
          setPendingBlocker(objectId);
        }
      } else {
        // Second click: assign to an attacker
        if (combatAttackerIds.includes(objectId)) {
          assignBlocker(pendingBlocker, objectId);
          setPendingBlocker(null);
        }
      }
    },
    [pendingBlocker, validBlockerIds, combatAttackerIds, assignBlocker],
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
    dispatch({ type: "DeclareAttackers", data: { attacks: [] } });
    clearCombatSelection();
  }, [dispatch, clearCombatSelection]);

  const handleConfirmAttackers = useCallback(() => {
    if (isMultiTarget) {
      setShowTargetPicker(true);
      return;
    }
    dispatch({
      type: "DeclareAttackers",
      data: { attacks: buildAttacks(selectedAttackers, gameState, playerId) },
    });
    clearCombatSelection();
  }, [dispatch, selectedAttackers, clearCombatSelection, isMultiTarget, gameState, playerId]);

  const handleTargetPickerConfirm = useCallback(
    (attacks: [ObjectId, AttackTarget][]) => {
      setShowTargetPicker(false);
      dispatch({ type: "DeclareAttackers", data: { attacks } });
      clearCombatSelection();
    },
    [dispatch, clearCombatSelection],
  );

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
      <>
        <AttackerControls
          onAttackAll={handleAttackAll}
          onSkip={handleSkip}
          onConfirm={handleConfirmAttackers}
          attackerCount={selectedAttackers.length}
        />
        {showTargetPicker && (
          <AttackTargetPicker
            validTargets={validAttackTargets}
            selectedAttackers={selectedAttackers}
            onConfirm={handleTargetPickerConfirm}
            onCancel={() => setShowTargetPicker(false)}
          />
        )}
      </>
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
