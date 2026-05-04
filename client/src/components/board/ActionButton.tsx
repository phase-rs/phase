import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { AttackTarget, ObjectId, WaitingFor } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { dispatchAction, dispatchResolveAll } from "../../game/dispatch.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { usePhaseInfo } from "../../hooks/usePhaseInfo.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
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
  const stackLength = useGameStore((s) => s.gameState?.stack.length ?? 0);
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

  const canCompanionToHand = useGameStore((s) =>
    s.legalActions.some((a) => a.type === "CompanionToHand"),
  );

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

  const actionPending = useMultiplayerStore((s) => s.actionPending);
  const idle = mode === "hidden" && !isEndingTurn;
  const blocked = idle || actionPending;
  const panelClassName =
    "flex max-w-[min(32rem,calc(100vw-1.25rem))] flex-row flex-wrap items-center justify-end gap-1.5 rounded-[22px] border border-white/10 bg-slate-950/72 p-2 shadow-[0_24px_64px_rgba(15,23,42,0.52)] backdrop-blur-xl lg:max-w-none [@media(max-height:500px)]:gap-1 [@media(max-height:500px)]:p-1 [@media(max-height:500px)]:rounded-[14px]";
  const primaryButtonClass = "min-w-[10.5rem] lg:min-w-[12rem] [@media(max-height:500px)]:!min-w-[5.5rem] [@media(max-height:500px)]:!min-h-7 [@media(max-height:500px)]:!px-2 [@media(max-height:500px)]:!py-0.5 [@media(max-height:500px)]:!text-[10px]";
  const secondaryButtonClass = "min-w-[8rem] [@media(max-height:500px)]:!min-w-[4.5rem] [@media(max-height:500px)]:!min-h-7 [@media(max-height:500px)]:!px-2 [@media(max-height:500px)]:!py-0.5 [@media(max-height:500px)]:!text-[10px]";

  return (
    <>
      <div className={panelClassName}>
        {mode === "combat-attackers" && !isEndingTurn && (
          <>
            <button
              disabled={actionPending}
              onClick={() => {
                if (selectedAttackers.length > 0) {
                  handleClearAttackers();
                } else {
                  selectAllAttackers(validAttackerIds);
                }
              }}
              className={gameButtonClass({ tone: "amber", size: "md", disabled: actionPending, className: secondaryButtonClass })}
            >
              {selectedAttackers.length > 0 ? "Clear Attackers" : "Attack with All"}
            </button>
            {selectedAttackers.length > 0 ? (
              <button
                disabled={actionPending}
                onClick={handleConfirmAttackers}
                className={gameButtonClass({ tone: "emerald", size: "md", disabled: actionPending, className: primaryButtonClass })}
              >
                Confirm Attackers ({selectedAttackers.length})
              </button>
            ) : (
              <button
                disabled={actionPending}
                onClick={() => handleSkipConfirm("attackers")}
                className={gameButtonClass({ tone: "slate", size: "md", disabled: actionPending, className: primaryButtonClass })}
              >
                {skipArmed === "attackers"
                  ? "Tap Again: Attack with None"
                  : "Attack with None"}
              </button>
            )}
          </>
        )}

        {mode === "combat-blockers" && !isEndingTurn && (
          <>
            {blockerAssignments.size > 0 ? (
              <>
                <button
                  disabled={actionPending}
                  onClick={handleConfirmBlockers}
                  className={gameButtonClass({ tone: "emerald", size: "md", disabled: actionPending, className: primaryButtonClass })}
                >
                  Confirm Blockers ({blockerAssignments.size})
                </button>
                <button
                  disabled={actionPending}
                  onClick={handleClearBlockers}
                  className={gameButtonClass({ tone: "neutral", size: "md", disabled: actionPending, className: secondaryButtonClass })}
                >
                  Reset Blocks
                </button>
              </>
            ) : (
              <button
                disabled={actionPending}
                onClick={() => handleSkipConfirm("blockers")}
                className={gameButtonClass({ tone: "slate", size: "md", disabled: actionPending, className: primaryButtonClass })}
              >
                {skipArmed === "blockers"
                  ? "Tap Again: Block with None"
                  : "Block with None"}
              </button>
            )}
            {pendingBlocker !== null && (
              <div className="absolute bottom-full right-0 mb-3 whitespace-nowrap rounded-full border border-cyan-300/25 bg-cyan-950/80 px-4 py-2 text-sm font-medium text-cyan-100 shadow-lg backdrop-blur-xl">
                Select the attacker this blocker should defend against
              </div>
            )}
          </>
        )}

        {mode === "priority-stack" && !isEndingTurn && (
          <>
            {canCompanionToHand && (
              <button
                disabled={actionPending}
                onClick={() => dispatchAction({ type: "CompanionToHand" })}
                className={gameButtonClass({ tone: "amber", size: "md", disabled: actionPending, className: secondaryButtonClass })}
              >
                Companion to Hand
              </button>
            )}
            <button
              disabled={actionPending}
              onClick={() => dispatchAction({ type: "PassPriority" })}
              className={gameButtonClass({ tone: "blue", size: "md", disabled: actionPending, className: primaryButtonClass })}
            >
              Resolve
            </button>
            <button
              disabled={actionPending}
              onClick={() => {
                const playerCount = useGameStore.getState().gameState?.players?.length ?? 2;
                const aiSeats = usePreferencesStore.getState().aiSeats;
                const seats = Array.from({ length: playerCount - 1 }, (_, i) => ({
                  playerId: i + 1,
                  difficulty: aiSeats[i]?.difficulty ?? "Medium",
                }));
                dispatchResolveAll(playerId, seats);
              }}
              className={gameButtonClass({ tone: "slate", size: "md", disabled: actionPending, className: secondaryButtonClass })}
            >
              Resolve All
            </button>
          </>
        )}

        {(mode === "priority-empty" || idle) && !isEndingTurn && (
          <>
            {canCompanionToHand && !idle && (
              <button
                disabled={actionPending}
                onClick={() => dispatchAction({ type: "CompanionToHand" })}
                className={gameButtonClass({ tone: "amber", size: "md", disabled: actionPending, className: secondaryButtonClass })}
              >
                Companion to Hand
              </button>
            )}
            <button
              disabled={blocked}
              onClick={() => dispatchAction({ type: "PassPriority" })}
              className={gameButtonClass({
                tone: "emerald",
                size: "md",
                disabled: blocked,
                className: primaryButtonClass,
              })}
            >
              {idle ? "Waiting" : advanceLabel}
            </button>
            <button
              disabled={blocked}
              onClick={() => dispatchAction({ type: "SetAutoPass", data: { mode: { type: "UntilEndOfTurn" } } })}
              className={`group relative ${gameButtonClass({ tone: "slate", size: "md", disabled: blocked, className: secondaryButtonClass })}`}
            >
              <span className="flex items-center gap-1">
                Pass
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4">
                  <path fillRule="evenodd" d="M2 10a.75.75 0 0 1 .75-.75h12.59l-2.1-1.95a.75.75 0 1 1 1.02-1.1l3.5 3.25a.75.75 0 0 1 0 1.1l-3.5 3.25a.75.75 0 1 1-1.02-1.1l2.1-1.95H2.75A.75.75 0 0 1 2 10Z" clipRule="evenodd" />
                </svg>
              </span>
              <span
                role="tooltip"
                className="pointer-events-none absolute bottom-full right-0 z-50 mb-2 hidden w-48 rounded-md border border-white/10 bg-slate-950/95 px-3 py-2 text-left text-[11px] leading-snug font-medium text-slate-100 shadow-2xl shadow-black/40 backdrop-blur-xl group-hover:block group-focus-visible:block"
              >
                Pass until end step — skip all remaining priority in this turn
              </span>
            </button>
          </>
        )}

        {isEndingTurn && (
          <button
            disabled={actionPending}
            onClick={() => dispatchAction({ type: "CancelAutoPass" })}
            className={gameButtonClass({ tone: "amber", size: "md", disabled: actionPending, className: `${primaryButtonClass} animate-pulse` })}
          >
            <span className="flex items-center gap-1.5">
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4 animate-spin">
                <path fillRule="evenodd" d="M15.312 11.424a5.5 5.5 0 0 1-9.201 2.466l-.312-.311h2.451a.75.75 0 0 0 0-1.5H4.5a.75.75 0 0 0-.75.75v3.75a.75.75 0 0 0 1.5 0v-2.033l.364.363a7 7 0 0 0 11.712-3.138.75.75 0 0 0-1.449-.39Zm-10.624-2.85a5.5 5.5 0 0 1 9.201-2.465l.312.31H11.75a.75.75 0 0 0 0 1.5h3.75a.75.75 0 0 0 .75-.75V3.42a.75.75 0 0 0-1.5 0v2.033l-.364-.364A7 7 0 0 0 3.074 8.227a.75.75 0 0 0 1.449.39l.165-.044Z" clipRule="evenodd" />
              </svg>
              Auto-Passing to End Step...
            </span>
          </button>
        )}
      </div>

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
