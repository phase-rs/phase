import { useCallback, useEffect, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";

import type { ObjectId, WaitingFor } from "../../adapter/types.ts";
import { PLAYER_ID } from "../../constants/game.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { usePhaseInfo } from "../../hooks/usePhaseInfo.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";

type ActionButtonMode =
  | "combat-attackers"
  | "combat-blockers"
  | "priority-stack"
  | "priority-empty"
  | "hidden";

function getActionButtonMode(
  waitingFor: WaitingFor | null | undefined,
  stackLength: number,
): ActionButtonMode {
  if (!waitingFor) return "hidden";

  if (
    waitingFor.type === "DeclareAttackers" &&
    waitingFor.data.player === PLAYER_ID
  ) {
    return "combat-attackers";
  }
  if (
    waitingFor.type === "DeclareBlockers" &&
    waitingFor.data.player === PLAYER_ID
  ) {
    return "combat-blockers";
  }
  if (
    waitingFor.type === "Priority" &&
    waitingFor.data.player === PLAYER_ID
  ) {
    return stackLength > 0 ? "priority-stack" : "priority-empty";
  }

  return "hidden";
}

export function ActionButton() {
  const waitingFor = useGameStore((s) => s.waitingFor);
  const stackLength = useGameStore((s) => s.gameState?.stack.length ?? 0);
  const gameState = useGameStore((s) => s.gameState);

  const selectedAttackers = useUiStore((s) => s.selectedAttackers);
  const selectAllAttackers = useUiStore((s) => s.selectAllAttackers);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const assignBlocker = useUiStore((s) => s.assignBlocker);
  const clearCombatSelection = useUiStore((s) => s.clearCombatSelection);
  const setCombatMode = useUiStore((s) => s.setCombatMode);
  const setCombatClickHandler = useUiStore((s) => s.setCombatClickHandler);

  const { advanceLabel } = usePhaseInfo();

  const mode = getActionButtonMode(waitingFor, stackLength);

  // Skip-confirm state for No Attacks / No Blocks
  const [skipArmed, setSkipArmed] = useState<"attackers" | "blockers" | null>(null);
  const skipTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Pending blocker for two-click assignment
  const [pendingBlocker, setPendingBlocker] = useState<ObjectId | null>(null);

  // Resolve All ref
  const resolveAllRef = useRef(false);

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

  // Blocker click handler
  const handleBlockerClick = useCallback(
    (objectId: ObjectId) => {
      if (!gameState) return;

      if (pendingBlocker === null) {
        const obj = gameState.objects[objectId];
        if (
          obj &&
          obj.controller === PLAYER_ID &&
          obj.card_types.core_types.includes("Creature")
        ) {
          setPendingBlocker(objectId);
        }
      } else {
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

  // Compute valid attacker IDs
  const validAttackerIds = (() => {
    if (!gameState || mode !== "combat-attackers") return [];
    const ids: ObjectId[] = [];
    for (const idStr of gameState.battlefield) {
      const obj = gameState.objects[idStr];
      if (
        obj &&
        obj.controller === PLAYER_ID &&
        obj.card_types.core_types.includes("Creature") &&
        !obj.tapped
      ) {
        ids.push(obj.id);
      }
    }
    return ids;
  })();

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
        dispatchAction({ type: "DeclareAttackers", data: { attacker_ids: [] } });
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
    dispatchAction({
      type: "DeclareAttackers",
      data: { attacker_ids: selectedAttackers },
    });
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

  const endTurnRef = useRef(false);

  async function endTurn() {
    endTurnRef.current = true;
    const startTurn = useGameStore.getState().gameState?.turn_number ?? 0;
    for (let i = 0; i < 50; i++) {
      if (!endTurnRef.current) break;
      const current = useGameStore.getState();
      const currentTurn = current.gameState?.turn_number ?? 0;
      if (currentTurn !== startTurn) break;
      if (current.waitingFor?.type !== "Priority") break;
      if (current.waitingFor.data.player !== PLAYER_ID) break;
      await dispatchAction({ type: "PassPriority" });
    }
    endTurnRef.current = false;
  }

  async function resolveAll() {
    resolveAllRef.current = true;
    const initialStackLength =
      useGameStore.getState().gameState?.stack.length ?? 0;
    for (let i = 0; i < initialStackLength; i++) {
      if (!resolveAllRef.current) break;
      const current = useGameStore.getState();
      const currentStack = current.gameState?.stack.length ?? 0;
      if (currentStack === 0) break;
      if (currentStack > initialStackLength - i) break;
      if (current.waitingFor?.type !== "Priority") break;
      if (current.waitingFor.data.player !== PLAYER_ID) break;
      await dispatchAction({ type: "PassPriority" });
    }
    resolveAllRef.current = false;
  }

  const visible = mode !== "hidden";

  return (
    <AnimatePresence>
      {visible && (
        <motion.div
          key={mode}
          initial={{ opacity: 0, y: 20 }}
          animate={{ opacity: 1, y: 0 }}
          exit={{ opacity: 0, y: 20 }}
          transition={{ duration: 0.15 }}
          className="fixed bottom-28 right-4 z-30 flex items-center gap-2"
        >
          {mode === "combat-attackers" && (
            <>
              <button
                onClick={() => {
                  if (selectedAttackers.length > 0) {
                    handleClearAttackers();
                  } else {
                    selectAllAttackers(validAttackerIds);
                  }
                }}
                className={gameButtonClass({ tone: "amber", size: "md" })}
              >
                {selectedAttackers.length > 0 ? "Clear Attackers" : "All Attack"}
              </button>
              {selectedAttackers.length > 0 ? (
                <button
                  onClick={handleConfirmAttackers}
                  className={gameButtonClass({ tone: "emerald", size: "md" })}
                >
                  Confirm Attackers ({selectedAttackers.length})
                </button>
              ) : (
                <button
                  onClick={() => handleSkipConfirm("attackers")}
                  className={gameButtonClass({ tone: "slate", size: "md" })}
                >
                  {skipArmed === "attackers"
                    ? "Tap again: No Attacks"
                    : "No Attacks"}
                </button>
              )}
            </>
          )}

          {mode === "combat-blockers" && (
            <>
              {blockerAssignments.size > 0 ? (
                <>
                  <button
                    onClick={handleConfirmBlockers}
                    className={gameButtonClass({ tone: "emerald", size: "md" })}
                  >
                    Confirm Blockers ({blockerAssignments.size})
                  </button>
                  <button
                    onClick={handleClearBlockers}
                    className={gameButtonClass({ tone: "neutral", size: "md" })}
                  >
                    Clear Blocks
                  </button>
                </>
              ) : (
                <button
                  onClick={() => handleSkipConfirm("blockers")}
                  className={gameButtonClass({ tone: "slate", size: "md" })}
                >
                  {skipArmed === "blockers"
                    ? "Tap again: No Blocks"
                    : "No Blocks"}
                </button>
              )}
              {pendingBlocker !== null && (
                <div className="absolute -top-10 rounded-lg bg-blue-900/80 px-4 py-2 text-sm font-medium text-blue-200 shadow-lg">
                  Click an attacker to assign blocker
                </div>
              )}
            </>
          )}

          {mode === "priority-stack" && (
            <>
              <button
                onClick={() => dispatchAction({ type: "PassPriority" })}
                className={gameButtonClass({ tone: "blue", size: "md" })}
              >
                Resolve
              </button>
              <button
                onClick={resolveAll}
                className={gameButtonClass({ tone: "slate", size: "md" })}
              >
                Resolve All
              </button>
            </>
          )}

          {mode === "priority-empty" && (
            <>
              <button
                onClick={() => dispatchAction({ type: "PassPriority" })}
                className={gameButtonClass({
                  tone: advanceLabel === "Done" ? "blue" : "emerald",
                  size: "md",
                })}
              >
                {advanceLabel}
              </button>
              <button
                onClick={endTurn}
                className={gameButtonClass({ tone: "slate", size: "md" })}
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
        </motion.div>
      )}
    </AnimatePresence>
  );
}
