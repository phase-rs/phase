import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";

import type { AttackTarget, ObjectId, WaitingFor } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { usePhaseInfo } from "../../hooks/usePhaseInfo.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { buildAttacks, hasMultipleAttackTargets, getValidAttackTargets } from "../../utils/combat.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { AttackTargetPicker } from "../controls/AttackTargetPicker.tsx";

type ActionButtonMode =
  | "combat-attackers"
  | "combat-blockers"
  | "priority-stack"
  | "priority-empty"
  | "hidden";

function getActionButtonMode(
  waitingFor: WaitingFor | null | undefined,
  stackLength: number,
  currentPlayerId: number,
): ActionButtonMode {
  if (!waitingFor) return "hidden";

  if (
    waitingFor.type === "DeclareAttackers" &&
    waitingFor.data.player === currentPlayerId
  ) {
    return "combat-attackers";
  }
  if (
    waitingFor.type === "DeclareBlockers" &&
    waitingFor.data.player === currentPlayerId
  ) {
    return "combat-blockers";
  }
  if (
    waitingFor.type === "Priority" &&
    waitingFor.data.player === currentPlayerId
  ) {
    return stackLength > 0 ? "priority-stack" : "priority-empty";
  }

  return "hidden";
}

export function ActionButton() {
  const playerId = usePlayerId();
  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const stackLength = gameState?.stack.length ?? 0;
  const combatAttackers = useGameStore(
    (s) => s.gameState?.combat?.attackers,
  );
  const combatAttackerIds = useMemo(
    () => combatAttackers?.map((a) => a.object_id) ?? [],
    [combatAttackers],
  );

  const selectedAttackers = useUiStore((s) => s.selectedAttackers);
  const selectAllAttackers = useUiStore((s) => s.selectAllAttackers);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const assignBlocker = useUiStore((s) => s.assignBlocker);
  const removeBlockerAssignment = useUiStore((s) => s.removeBlockerAssignment);
  const clearCombatSelection = useUiStore((s) => s.clearCombatSelection);
  const setCombatMode = useUiStore((s) => s.setCombatMode);
  const setCombatClickHandler = useUiStore((s) => s.setCombatClickHandler);

  const { advanceLabel } = usePhaseInfo();

  const mode = getActionButtonMode(waitingFor, stackLength, playerId);

  // Skip-confirm state for No Attacks / No Blocks
  const [skipArmed, setSkipArmed] = useState<"attackers" | "blockers" | null>(null);
  const skipTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Pending blocker for two-click assignment
  const [pendingBlocker, setPendingBlocker] = useState<ObjectId | null>(null);

  // Attack target picker visibility (multiplayer)
  const [showTargetPicker, setShowTargetPicker] = useState(false);
  const isMultiTarget = hasMultipleAttackTargets(gameState);
  const validAttackTargets = getValidAttackTargets(gameState);

  // Reset skip-confirm when mode changes
  useEffect(() => {
    setSkipArmed(null);
    if (skipTimerRef.current) {
      clearTimeout(skipTimerRef.current);
      skipTimerRef.current = null;
    }
  }, [mode]);

  // Set combat mode and register click handlers
  useEffect(() => {
    if (mode === "combat-attackers") {
      setCombatMode("attackers");
    } else if (mode === "combat-blockers") {
      setCombatMode("blockers");
    }
    return () => {
      if (mode === "combat-attackers" || mode === "combat-blockers") {
        clearCombatSelection();
      }
    };
  }, [mode, setCombatMode, clearCombatSelection]);

  // Valid blocker IDs from engine
  const validBlockerIds = useMemo(
    () =>
      waitingFor?.type === "DeclareBlockers"
        ? (waitingFor.data.valid_blocker_ids ?? [])
        : [],
    [waitingFor],
  );

  // Per-blocker valid attacker targets from engine
  const validBlockTargets = useMemo(
    () =>
      waitingFor?.type === "DeclareBlockers"
        ? (waitingFor.data.valid_block_targets ?? {})
        : {},
    [waitingFor],
  );

  // Blocker click handler
  const handleBlockerClick = useCallback(
    (objectId: ObjectId) => {
      // Click an already-assigned blocker to unassign
      if (blockerAssignments.has(objectId)) {
        removeBlockerAssignment(objectId);
        return;
      }

      if (pendingBlocker === null) {
        // First click: select a valid blocker (must have at least one valid target)
        if (validBlockerIds.includes(objectId) && validBlockTargets[objectId]?.length > 0) {
          setPendingBlocker(objectId);
        }
      } else {
        // Second click: assign to an attacker (only if engine says this pair is valid)
        const validTargetsForBlocker = validBlockTargets[pendingBlocker] ?? [];
        if (combatAttackerIds.includes(objectId) && validTargetsForBlocker.includes(objectId)) {
          assignBlocker(pendingBlocker, objectId);
          setPendingBlocker(null);
        }
      }
    },
    [pendingBlocker, validBlockerIds, validBlockTargets, combatAttackerIds, assignBlocker, blockerAssignments, removeBlockerAssignment],
  );

  useEffect(() => {
    if (mode === "combat-blockers") {
      setCombatClickHandler(handleBlockerClick);
    }
    return () => {
      if (mode === "combat-blockers") {
        setCombatClickHandler(null);
      }
    };
  }, [mode, handleBlockerClick, setCombatClickHandler]);

  // Reset pending blocker on mode change
  useEffect(() => {
    setPendingBlocker(null);
  }, [mode]);

  // Valid attacker IDs from engine
  const validAttackerIds =
    waitingFor?.type === "DeclareAttackers"
      ? (waitingFor.data.valid_attacker_ids ?? [])
      : [];

  // -- Handlers --

  function handleSkipConfirm(skipType: "attackers" | "blockers") {
    if (skipArmed === skipType) {
      // Second tap: dispatch
      if (skipTimerRef.current) {
        clearTimeout(skipTimerRef.current);
        skipTimerRef.current = null;
      }
      setSkipArmed(null);
      if (skipType === "attackers") {
        dispatchAction({ type: "DeclareAttackers", data: { attacks: [] } });
      } else {
        dispatchAction({ type: "DeclareBlockers", data: { assignments: [] } });
      }
    } else {
      // First tap: arm
      setSkipArmed(skipType);
      skipTimerRef.current = setTimeout(() => {
        setSkipArmed(null);
        skipTimerRef.current = null;
      }, 1200);
    }
  }

  function handleConfirmAttackers() {
    if (isMultiTarget) {
      setShowTargetPicker(true);
      return;
    }
    dispatchAction({
      type: "DeclareAttackers",
      data: { attacks: buildAttacks(selectedAttackers, gameState, playerId) },
    });
  }

  function handleTargetPickerConfirm(attacks: [ObjectId, AttackTarget][]) {
    setShowTargetPicker(false);
    dispatchAction({ type: "DeclareAttackers", data: { attacks } });
  }

  function handleConfirmBlockers() {
    dispatchAction({
      type: "DeclareBlockers",
      data: { assignments: Array.from(blockerAssignments.entries()) },
    });
  }

  function handleClearAttackers() {
    clearCombatSelection();
    setCombatMode("attackers");
  }

  function handleClearBlockers() {
    clearCombatSelection();
    setCombatMode("blockers");
  }

  // Read auto-pass state from engine
  const autoPass = gameState?.auto_pass?.[playerId];
  const isEndingTurn = autoPass?.type === "UntilEndOfTurn";
  const isResolvingAll = autoPass?.type === "UntilStackEmpty";

  const visible = mode !== "hidden" || isEndingTurn;
  const panelClassName =
    "fixed z-30 flex max-w-[min(25rem,calc(100vw-1.25rem))] flex-col items-stretch gap-1.5 rounded-[14px] border border-white/10 bg-[#0b1020]/88 p-1.5 shadow-[0_20px_48px_rgba(0,0,0,0.44)] backdrop-blur-md sm:max-w-none sm:flex-row sm:items-center";
  const primaryButtonClass = "w-full sm:w-auto sm:min-w-[10rem]";
  const secondaryButtonClass = "w-full sm:w-auto";

  return (
    <AnimatePresence>
      {visible && (
        <motion.div
          key={mode}
          initial={{ opacity: 0, y: 20 }}
          animate={{ opacity: 1, y: 0 }}
          exit={{ opacity: 0, y: 20 }}
          transition={{ duration: 0.15 }}
          className={panelClassName}
          style={{
            bottom: "calc(env(safe-area-inset-bottom) + 7rem)",
            right: "calc(env(safe-area-inset-right) + 1rem + var(--game-right-rail-offset, 0px))",
          }}
        >
          {mode === "combat-attackers" && !isEndingTurn && (
            <>
              <button
                onClick={() => {
                  if (selectedAttackers.length > 0) {
                    handleClearAttackers();
                  } else {
                    selectAllAttackers(validAttackerIds);
                  }
                }}
                className={gameButtonClass({ tone: "amber", size: "md", className: secondaryButtonClass })}
              >
                {selectedAttackers.length > 0 ? "Clear Attackers" : "All Attack"}
              </button>
              {selectedAttackers.length > 0 ? (
                <button
                  onClick={handleConfirmAttackers}
                  className={gameButtonClass({ tone: "emerald", size: "md", className: primaryButtonClass })}
                >
                  Confirm Attackers ({selectedAttackers.length})
                </button>
              ) : (
                <button
                  onClick={() => handleSkipConfirm("attackers")}
                  className={gameButtonClass({ tone: "slate", size: "md", className: primaryButtonClass })}
                >
                  {skipArmed === "attackers"
                    ? "Tap again: No Attacks"
                    : "No Attacks"}
                </button>
              )}
            </>
          )}

          {mode === "combat-blockers" && !isEndingTurn && (
            <>
              {blockerAssignments.size > 0 ? (
                <>
                  <button
                    onClick={handleConfirmBlockers}
                    className={gameButtonClass({ tone: "emerald", size: "md", className: primaryButtonClass })}
                  >
                    Confirm Blockers ({blockerAssignments.size})
                  </button>
                  <button
                    onClick={handleClearBlockers}
                    className={gameButtonClass({ tone: "neutral", size: "md", className: secondaryButtonClass })}
                  >
                    Clear Blocks
                  </button>
                </>
              ) : (
                <button
                  onClick={() => handleSkipConfirm("blockers")}
                  className={gameButtonClass({ tone: "slate", size: "md", className: primaryButtonClass })}
                >
                  {skipArmed === "blockers"
                    ? "Tap again: No Blocks"
                    : "No Blocks"}
                </button>
              )}
              {pendingBlocker !== null && (
                <div className="absolute bottom-full mb-3 right-0 rounded-lg bg-blue-900/80 px-4 py-2 text-sm font-medium text-blue-200 shadow-lg whitespace-nowrap">
                  Click an attacker to assign blocker
                </div>
              )}
            </>
          )}

          {mode === "priority-stack" && !isEndingTurn && (
            <>
              <button
                onClick={() => dispatchAction({ type: "PassPriority" })}
                className={gameButtonClass({ tone: "blue", size: "md", className: primaryButtonClass })}
              >
                Resolve
              </button>
              <button
                onClick={() => dispatchAction({ type: "SetAutoPass", data: { mode: { type: "UntilStackEmpty" } } })}
                className={gameButtonClass({ tone: "slate", size: "md", className: secondaryButtonClass })}
              >
                Resolve All
              </button>
            </>
          )}

          {mode === "priority-empty" && !isEndingTurn && (
            <>
              <button
                onClick={() => dispatchAction({ type: "PassPriority" })}
                className={gameButtonClass({
                  tone: "emerald",
                  size: "md",
                  className: primaryButtonClass,
                })}
              >
                {advanceLabel}
              </button>
              <button
                onClick={() => dispatchAction({ type: "SetAutoPass", data: { mode: { type: "UntilEndOfTurn" } } })}
                className={gameButtonClass({ tone: "slate", size: "md", className: secondaryButtonClass })}
              >
                <span className="flex items-center gap-1">
                  End Turn
                  <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4">
                    <path fillRule="evenodd" d="M2 10a.75.75 0 0 1 .75-.75h12.59l-2.1-1.95a.75.75 0 1 1 1.02-1.1l3.5 3.25a.75.75 0 0 1 0 1.1l-3.5 3.25a.75.75 0 1 1-1.02-1.1l2.1-1.95H2.75A.75.75 0 0 1 2 10Z" clipRule="evenodd" />
                  </svg>
                </span>
              </button>
            </>
          )}

          {isEndingTurn && (
            <button
              onClick={() => dispatchAction({ type: "CancelAutoPass" })}
              className={gameButtonClass({ tone: "amber", size: "md", className: `${primaryButtonClass} animate-pulse` })}
            >
              <span className="flex items-center gap-1.5">
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4 animate-spin">
                  <path fillRule="evenodd" d="M15.312 11.424a5.5 5.5 0 0 1-9.201 2.466l-.312-.311h2.451a.75.75 0 0 0 0-1.5H4.5a.75.75 0 0 0-.75.75v3.75a.75.75 0 0 0 1.5 0v-2.033l.364.363a7 7 0 0 0 11.712-3.138.75.75 0 0 0-1.449-.39Zm-10.624-2.85a5.5 5.5 0 0 1 9.201-2.465l.312.31H11.75a.75.75 0 0 0 0 1.5h3.75a.75.75 0 0 0 .75-.75V3.42a.75.75 0 0 0-1.5 0v2.033l-.364-.364A7 7 0 0 0 3.074 8.227a.75.75 0 0 0 1.449.39l.165-.044Z" clipRule="evenodd" />
                </svg>
                Ending Turn...
              </span>
            </button>
          )}
        </motion.div>
      )}

      {showTargetPicker && (
        <AttackTargetPicker
          validTargets={validAttackTargets}
          selectedAttackers={selectedAttackers}
          onConfirm={handleTargetPickerConfirm}
          onCancel={() => setShowTargetPicker(false)}
        />
      )}
    </AnimatePresence>
  );
}
