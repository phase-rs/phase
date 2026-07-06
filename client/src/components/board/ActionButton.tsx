import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { AttackTarget, ObjectId, WaitingFor } from "../../adapter/types.ts";
import { usePlayerId, useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { dispatchAction, dispatchResolveAll } from "../../game/dispatch.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { usePhaseInfo } from "../../hooks/usePhaseInfo.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DRAFT_BOT_AI_SEAT, useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { buildAttacks, hasMultipleAttackTargets, getValidAttackTargets } from "../../utils/combat.ts";
import { useBlockRequirements } from "../combat/useBlockRequirements.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";
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
  canAct: boolean,
): ActionButtonMode {
  if (!waitingFor || !canAct) return "hidden";

  if (waitingFor.type === "DeclareAttackers") {
    return "combat-attackers";
  }
  if (waitingFor.type === "DeclareBlockers") {
    return "combat-blockers";
  }
  if (waitingFor.type === "Priority") {
    return stackLength > 0 ? "priority-stack" : "priority-empty";
  }

  return "hidden";
}

export function ActionButton() {
  const { t } = useTranslation("game");
  const priorityTooltipId = useId();
  const resolveTooltipId = useId();
  const resolveAllTooltipId = useId();
  const passToEndTooltipId = useId();
  const playerId = usePlayerId();
  const canActForWaitingState = useCanActForWaitingState();
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

  // Engine-declared per-attacker minimum-blocker requirements (menace /
  // "blocked by N or more"). Used to block confirmation while any attacker is
  // under-assigned, so the player gets a clear message instead of an engine
  // rejection (CR 702.111b / CR 509.1b).
  const { byAttacker: blockRequirements } = useBlockRequirements();
  const incompleteBlockCount = useMemo(
    () => Array.from(blockRequirements.values()).filter((r) => r.status === "incomplete").length,
    [blockRequirements],
  );

  const canCompanionToHand = useGameStore((s) =>
    s.legalActions.some((a) => a.type === "CompanionToHand"),
  );

  const { advanceLabel } = usePhaseInfo();

  const mode = getActionButtonMode(waitingFor, stackLength, canActForWaitingState);

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
  const isEndingTurn = autoPass?.type === "UntilTurnBoundary";
  // Armed Arena-style "Resolve All" session (multiplayer): the engine is
  // auto-passing this seat's priority windows until the stack empties or
  // grows. Surfaced with the same pulsing cancel affordance as UntilTurnBoundary
  // so the player can revoke it between opponents' windows.
  const isResolvingStack = autoPass?.type === "UntilStackEmpty";
  const canActDuringAutoPass = mode === "combat-blockers";

  const actionPending = useMultiplayerStore((s) => s.actionPending);
  const isResolvingAll = useGameStore((s) => s.isResolvingAll);
  const actionBlocked = actionPending || isResolvingAll;
  const idle = mode === "hidden" && !isEndingTurn;
  const blocked = idle || actionBlocked;
  const panelClassName =
    "flex max-w-[min(32rem,calc(100vw-1.25rem))] flex-row flex-wrap items-center justify-end gap-1.5 rounded-[22px] border border-white/10 bg-slate-950/72 p-2 shadow-[0_24px_64px_rgba(15,23,42,0.52)] backdrop-blur-xl max-lg:portrait:w-full max-lg:portrait:max-w-none max-lg:portrait:flex-col max-lg:portrait:flex-nowrap max-lg:portrait:gap-1 max-lg:portrait:p-1.5 lg:max-w-none [@media(max-height:500px)]:gap-1 [@media(max-height:500px)]:p-1 [@media(max-height:500px)]:rounded-[14px]";
  const primaryButtonClass = "min-w-[10.5rem] max-lg:portrait:w-full max-lg:portrait:!min-w-0 lg:min-w-[12rem] [@media(max-height:500px)]:!min-w-[5.5rem] [@media(max-height:500px)]:!min-h-7 [@media(max-height:500px)]:!px-2 [@media(max-height:500px)]:!py-0.5 [@media(max-height:500px)]:!text-[10px]";
  const secondaryButtonClass = "min-w-[8rem] max-lg:portrait:w-full max-lg:portrait:!min-w-0 [@media(max-height:500px)]:!min-w-[4.5rem] [@media(max-height:500px)]:!min-h-7 [@media(max-height:500px)]:!px-2 [@media(max-height:500px)]:!py-0.5 [@media(max-height:500px)]:!text-[10px]";

  return (
    <>
      <div className={panelClassName} data-action-button-panel>
        {mode === "combat-attackers" && !isEndingTurn && (
          <>
            <button
              disabled={actionBlocked}
              onClick={() => {
                if (selectedAttackers.length > 0) {
                  handleClearAttackers();
                } else {
                  selectAllAttackers(validAttackerIds);
                }
              }}
              className={gameButtonClass({ tone: "amber", size: "md", disabled: actionBlocked, className: secondaryButtonClass })}
            >
              {selectedAttackers.length > 0 ? t("actionButton.clearAttackers") : t("actionButton.attackWithAll")}
            </button>
            {selectedAttackers.length > 0 ? (
              <button
                disabled={actionBlocked}
                onClick={handleConfirmAttackers}
                className={gameButtonClass({ tone: "emerald", size: "md", disabled: actionBlocked, className: primaryButtonClass })}
              >
                {t("actionButton.confirmAttackers", { count: selectedAttackers.length })}
              </button>
            ) : (
              <button
                disabled={actionBlocked}
                onClick={() => handleSkipConfirm("attackers")}
                className={gameButtonClass({ tone: "slate", size: "md", disabled: actionBlocked, className: primaryButtonClass })}
              >
                {skipArmed === "attackers"
                  ? t("actionButton.attackWithNoneConfirm")
                  : t("actionButton.attackWithNone")}
              </button>
            )}
          </>
        )}

        {mode === "combat-blockers" && (
          <>
            {blockerAssignments.size > 0 ? (
              <>
                <button
                  disabled={actionBlocked || incompleteBlockCount > 0}
                  onClick={handleConfirmBlockers}
                  className={gameButtonClass({ tone: "emerald", size: "md", disabled: actionBlocked || incompleteBlockCount > 0, className: primaryButtonClass })}
                >
                  {t("actionButton.confirmBlockers", { count: blockerAssignments.size })}
                </button>
                <button
                  disabled={actionBlocked}
                  onClick={handleClearBlockers}
                  className={gameButtonClass({ tone: "neutral", size: "md", disabled: actionBlocked, className: secondaryButtonClass })}
                >
                  {t("actionButton.resetBlocks")}
                </button>
              </>
            ) : (
              <button
                disabled={actionBlocked}
                onClick={() => handleSkipConfirm("blockers")}
                className={gameButtonClass({ tone: "slate", size: "md", disabled: actionBlocked, className: primaryButtonClass })}
              >
                {skipArmed === "blockers"
                  ? t("actionButton.blockWithNoneConfirm")
                  : t("actionButton.blockWithNone")}
              </button>
            )}
            {pendingBlocker !== null && (
              <div className="absolute bottom-full right-0 mb-3 whitespace-nowrap rounded-full border border-cyan-300/25 bg-cyan-950/80 px-4 py-2 text-sm font-medium text-cyan-100 shadow-lg backdrop-blur-xl">
                {t("actionButton.selectAttackerForBlocker")}
              </div>
            )}
            {pendingBlocker === null && incompleteBlockCount > 0 && (
              <div className="absolute bottom-full right-0 mb-3 whitespace-nowrap rounded-full border border-amber-300/30 bg-amber-950/85 px-4 py-2 text-sm font-medium text-amber-100 shadow-lg backdrop-blur-xl">
                {t("combat.blockIncomplete", { count: incompleteBlockCount })}
              </div>
            )}
          </>
        )}

        {mode === "priority-stack" && !isEndingTurn && (
          <>
            {canCompanionToHand && (
              <button
                disabled={actionBlocked}
                onClick={() => dispatchAction({ type: "CompanionToHand" })}
                className={gameButtonClass({ tone: "amber", size: "md", disabled: actionBlocked, className: secondaryButtonClass })}
              >
                {t("actionButton.companionToHand")}
              </button>
            )}
            <button
              disabled={actionBlocked}
              onClick={() => dispatchAction({ type: "PassPriority" })}
              aria-describedby={resolveTooltipId}
              className={gameButtonClass({ tone: "blue", size: "md", disabled: actionBlocked, className: `${primaryButtonClass} group relative` })}
            >
              {t("actionButton.resolve")}
              <GameplayTooltip id={resolveTooltipId}>
                {t("actionButton.resolveTooltip")}
              </GameplayTooltip>
            </button>
            <button
              disabled={actionBlocked}
              aria-busy={isResolvingAll}
              onClick={() => {
                const { gameState: gs, gameMode } = useGameStore.getState();
                // Only claim seats as AI-driven when an AI actually drives them,
                // mirroring the controller each mode installs. "ai": every
                // non-local seat (same prefs sourcing as scheduleBatchResolve).
                // Draft matches: only a Bot pairing has an AI seat, and it uses
                // the same binding installMatchRuntime gives the live controller.
                // Everything else — "local" hotseat above all (#4978) — gets an
                // empty list so dispatchResolveAll falls back to the per-seat
                // engine auto-yield instead of handing human seats to the AI.
                let seats: { playerId: number; difficulty: string }[] = [];
                if (gameMode === "ai") {
                  const playerCount = gs?.players?.length ?? 2;
                  const aiSeats = usePreferencesStore.getState().aiSeats;
                  seats = Array.from({ length: playerCount - 1 }, (_, i) => ({
                    playerId: i + 1,
                    difficulty: aiSeats[i]?.difficulty ?? "Medium",
                  }));
                } else if (
                  gameMode === "draft-match" &&
                  useMultiplayerDraftStore.getState().matchPairing?.type === "Bot"
                ) {
                  seats = [DRAFT_BOT_AI_SEAT];
                }
                dispatchResolveAll(playerId, seats);
              }}
              aria-describedby={resolveAllTooltipId}
              className={gameButtonClass({ tone: "slate", size: "md", disabled: actionBlocked, className: `${secondaryButtonClass} group relative` })}
            >
              {t("actionButton.resolveAll")}
              <GameplayTooltip id={resolveAllTooltipId}>
                {t("actionButton.resolveAllTooltip")}
              </GameplayTooltip>
            </button>
          </>
        )}

        {(mode === "priority-empty" || idle) && !isEndingTurn && !isResolvingStack && (
          <>
            {canCompanionToHand && !idle && (
              <button
                disabled={actionBlocked}
                onClick={() => dispatchAction({ type: "CompanionToHand" })}
                className={gameButtonClass({ tone: "amber", size: "md", disabled: actionBlocked, className: secondaryButtonClass })}
              >
                {t("actionButton.companionToHand")}
              </button>
            )}
            {/* In idle (no priority), the "who/why" narration lives in
                TurnStatusLine — rendering a disabled "Waiting" button here too
                would duplicate it (and an empty/relabeled disabled control is
                worse for screen readers). So show the actionable button only
                when the local player actually has the priority window. */}
            {!idle && (
              <button
                disabled={blocked}
                onClick={() => dispatchAction({ type: "PassPriority" })}
                aria-describedby={priorityTooltipId}
                className={gameButtonClass({
                  tone: "emerald",
                  size: "md",
                  disabled: blocked,
                  className: `${primaryButtonClass} group relative`,
                })}
              >
                {advanceLabel}
                <GameplayTooltip id={priorityTooltipId}>
                  {t("actionButton.priorityTooltip")}
                </GameplayTooltip>
              </button>
            )}
            <button
              disabled={blocked}
              onClick={() =>
                dispatchAction({
                  type: "SetAutoPass",
                  data: { mode: { type: "UntilTurnBoundary", until: "EndOfCurrentTurn" } },
                })
              }
              aria-describedby={passToEndTooltipId}
              className={`group relative ${gameButtonClass({ tone: "slate", size: "md", disabled: blocked, className: secondaryButtonClass })}`}
            >
              <span className="flex items-center gap-1">
                {t("actionButton.pass")}
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4">
                  <path fillRule="evenodd" d="M2 10a.75.75 0 0 1 .75-.75h12.59l-2.1-1.95a.75.75 0 1 1 1.02-1.1l3.5 3.25a.75.75 0 0 1 0 1.1l-3.5 3.25a.75.75 0 1 1-1.02-1.1l2.1-1.95H2.75A.75.75 0 0 1 2 10Z" clipRule="evenodd" />
                </svg>
              </span>
              <GameplayTooltip id={passToEndTooltipId} className="w-56">
                {t("actionButton.passToEndTooltip")}
              </GameplayTooltip>
            </button>
          </>
        )}

        {(isEndingTurn || isResolvingStack) && !canActDuringAutoPass && (
          <button
            disabled={actionBlocked}
            onClick={() => dispatchAction({ type: "CancelAutoPass" })}
            className={gameButtonClass({ tone: "amber", size: "md", disabled: actionBlocked, className: `${primaryButtonClass} animate-pulse` })}
          >
            <span className="flex items-center gap-1.5">
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4 animate-spin">
                <path fillRule="evenodd" d="M15.312 11.424a5.5 5.5 0 0 1-9.201 2.466l-.312-.311h2.451a.75.75 0 0 0 0-1.5H4.5a.75.75 0 0 0-.75.75v3.75a.75.75 0 0 0 1.5 0v-2.033l.364.363a7 7 0 0 0 11.712-3.138.75.75 0 0 0-1.449-.39Zm-10.624-2.85a5.5 5.5 0 0 1 9.201-2.465l.312.31H11.75a.75.75 0 0 0 0 1.5h3.75a.75.75 0 0 0 .75-.75V3.42a.75.75 0 0 0-1.5 0v2.033l-.364-.364A7 7 0 0 0 3.074 8.227a.75.75 0 0 0 1.449.39l.165-.044Z" clipRule="evenodd" />
              </svg>
              {isEndingTurn ? t("actionButton.autoPassing") : t("actionButton.resolvingStack")}
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
