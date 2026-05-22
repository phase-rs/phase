import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useEffect } from "react";

import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { renderDescription } from "../../utils/description.ts";
import type { GameObject } from "../../adapter/types.ts";
import { RichLabel } from "../mana/RichLabel.tsx";

export function TargetingOverlay() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const objects = useGameStore((s) => s.gameState?.objects);
  const selectedCardIds = useUiStore((s) => s.selectedCardIds);
  const clearSelectedCards = useUiStore((s) => s.clearSelectedCards);

  const isTargetSelection = waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection";
  const isCopyTargetChoice = waitingFor?.type === "CopyTargetChoice";
  const isCopyRetarget = waitingFor?.type === "CopyRetarget";
  const canKeepCurrentTargets = isCopyRetarget && waitingFor.data.target_slots.every((slot) => slot.current != null);
  const isExploreChoice = waitingFor?.type === "ExploreChoice";
  const isTapCreatureChoice = waitingFor?.type === "TapCreaturesForManaAbility" || waitingFor?.type === "TapCreaturesForSpellCost";
  const targetSlots = isTargetSelection ? waitingFor.data.target_slots : [];
  const selection = isTargetSelection ? waitingFor.data.selection : null;
  const currentTargetSlot = isCopyRetarget
    ? (waitingFor.data.current_slot ?? 0)
    : (selection?.current_slot ?? 0);
  const activeSlot = targetSlots[currentTargetSlot];
  const isOptionalCurrentSlot = activeSlot?.optional === true;
  const sourceId = waitingFor?.type === "TriggerTargetSelection"
    ? waitingFor.data.source_id
    : waitingFor?.type === "TargetSelection"
      ? waitingFor.data.pending_cast?.object_id
      : waitingFor?.type === "ExploreChoice"
        ? waitingFor.data.source_id
      : waitingFor?.type === "TapCreaturesForManaAbility"
        ? (waitingFor.data.pending_mana_ability as { source_id?: number } | undefined)?.source_id
      : waitingFor?.type === "TapCreaturesForSpellCost"
        ? waitingFor.data.pending_cast?.object_id
      : undefined;
  const sourceName = sourceId != null ? objects?.[sourceId]?.name : undefined;

  const inferredPrompt = buildInferredTargetPrompt({
    waitingFor: isTargetSelection ? waitingFor : null,
    objects,
    activeSlot,
    targetSlots,
    selection,
  });

  const triggerDescription = waitingFor?.type === "TriggerTargetSelection" && waitingFor.data.description
    ? renderDescription(waitingFor.data.description, sourceName ?? "this")
    : undefined;

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
  }, [dispatch]);

  const handleSkip = useCallback(() => {
    dispatch({ type: "ChooseTarget", data: { target: null } });
  }, [dispatch]);

  const handleConfirmTap = useCallback(() => {
    dispatch({ type: "SelectCards", data: { cards: selectedCardIds } });
  }, [dispatch, selectedCardIds]);

  useEffect(() => {
    if (!isTapCreatureChoice) {
      clearSelectedCards();
      return;
    }
    clearSelectedCards();
    return () => clearSelectedCards();
  }, [clearSelectedCards, isTapCreatureChoice]);

  if (!isTargetSelection && !isCopyTargetChoice && !isCopyRetarget && !isExploreChoice && !isTapCreatureChoice) return null;

  // Only show targeting UI for the human player
  if (!canActForWaitingState) return null;

  return (
    <AnimatePresence>
      <motion.div
        className="pointer-events-none fixed inset-0 z-40"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.2 }}
      >
        {/* Semi-transparent overlay (click-through so board cards remain clickable) */}
        <div className="absolute inset-0 bg-black/30" />

        {/* Instruction text */}
        <div className="absolute left-0 right-0 top-4 flex flex-col items-center gap-1">
          {sourceName && (
            <div className="rounded-md bg-gray-800/90 px-4 py-1 text-sm font-medium text-amber-300 shadow">
              {sourceName}
            </div>
          )}
            <div className="rounded-lg bg-gray-900/90 px-6 py-2 text-lg font-semibold text-cyan-400 shadow-lg">
            {isCopyTargetChoice
              ? "Choose a permanent to copy"
              : isCopyRetarget
                ? (() => {
                    const slots = waitingFor.data.target_slots;
                    const hasCurrent = slots.every((slot) => slot.current != null);
                    return slots.length > 1
                      ? `${hasCurrent ? "Retarget" : "Choose target for"} copy: slot ${Math.min(currentTargetSlot + 1, slots.length)} of ${slots.length}`
                      : hasCurrent ? "Choose new target for copy" : "Choose target for copy";
                  })()
              : isExploreChoice
                ? "Choose which creature explores next"
              : isTapCreatureChoice
                ? `Tap ${waitingFor.data.count} untapped creature${waitingFor.data.count > 1 ? "s" : ""}`
              : inferredPrompt ?? (
                targetSlots.length > 1
                  ? `Choose target ${Math.min(currentTargetSlot + 1, targetSlots.length)} of ${targetSlots.length}`
                  : "Choose a target"
              )}
            </div>
          {triggerDescription && (
            <div className="max-w-md rounded-md bg-gray-800/90 px-4 py-1 text-center text-xs text-gray-300 shadow">
              <RichLabel text={triggerDescription} size="xs" />
            </div>
          )}
        </div>

        {/* Player targets are handled by PlayerHud/OpponentHud glow + click */}

        <div className="pointer-events-auto absolute bottom-6 left-0 right-0 flex justify-center gap-4">
          {(waitingFor.type === "TargetSelection" || waitingFor.type === "TapCreaturesForSpellCost") && (
            <button
              onClick={handleCancel}
              className="rounded-lg bg-gray-700 px-6 py-2 font-semibold text-gray-200 shadow-lg transition hover:bg-gray-600"
            >
              Cancel
            </button>
          )}
          {isTapCreatureChoice && (
            <button
              onClick={handleConfirmTap}
              disabled={selectedCardIds.length !== waitingFor.data.count}
              className="rounded-lg bg-emerald-700 px-6 py-2 font-semibold text-gray-100 shadow-lg transition hover:bg-emerald-600 disabled:cursor-not-allowed disabled:bg-gray-700 disabled:text-gray-400"
            >
              Confirm Tap ({selectedCardIds.length}/{waitingFor.data.count})
            </button>
          )}
          {canKeepCurrentTargets && (
            <button
              onClick={() =>
                dispatch({
                  type: "KeepAllCopyTargets",
                })
              }
              className="rounded-lg bg-emerald-700 px-6 py-2 font-semibold text-gray-100 shadow-lg transition hover:bg-emerald-600"
            >
              Keep Current Targets
            </button>
          )}
          {isOptionalCurrentSlot && (
            <button
              onClick={handleSkip}
              className="rounded-lg bg-amber-700 px-6 py-2 font-semibold text-gray-100 shadow-lg transition hover:bg-amber-600"
            >
              Skip
            </button>
          )}
        </div>
      </motion.div>
    </AnimatePresence>
  );
}

type TargetingPromptParams = {
  waitingFor: {
    type: "TargetSelection" | "TriggerTargetSelection" | "ExploreChoice" | "CopyTargetChoice" | "TapCreaturesForManaAbility" | "TapCreaturesForSpellCost";
    data: {
      target_slots?: { legal_targets: { Object?: number; Player?: number }[]; optional?: boolean }[];
      selection?: { current_slot: number };
      player?: number;
    };
  } | null;
  objects?: Record<number, GameObject> | undefined;
  activeSlot: { legal_targets: { Object?: number; Player?: number }[]; optional?: boolean } | undefined;
  targetSlots: { legal_targets: { Object?: number; Player?: number }[]; optional?: boolean }[];
  selection: { current_slot: number } | null;
};

function buildInferredTargetPrompt({
  waitingFor,
  objects,
  activeSlot,
  targetSlots,
  selection,
}: TargetingPromptParams): string | null {
  if (!waitingFor) return null;
  if (waitingFor.type !== "TargetSelection" && waitingFor.type !== "TriggerTargetSelection") return null;
  if (!selection) return null;

  if (!activeSlot) return null;
  if (activeSlot.legal_targets.length === 0) {
    return "No legal targets available";
  }

  const targetNouns = inferTargetNoun(activeSlot.legal_targets, objects);
  const targetWord = targetNouns ? targetNouns : "target";
  const maybePrefix = selection && targetSlots.length === 1 && activeSlot.optional ? "up to one" : "a";

  if (waitingFor.type === "TriggerTargetSelection") {
    return `${maybePrefix} ${targetWord}`;
  }

  if (targetSlots.length <= 1) {
    return `${maybePrefix} ${targetWord}`;
  }

  return `Choose target ${Math.min(selection.current_slot + 1, targetSlots.length)} of ${targetSlots.length}`;
}

function inferTargetNoun(
  targets: { Object?: number; Player?: number }[],
  objects?: Record<number, GameObject>,
): string | null {
  const allPlayers = targets.every((target) => "Player" in target);
  if (allPlayers) return "player";

  const objectTargets = targets.flatMap((target) =>
    typeof target.Object === "number" ? [objects?.[target.Object]].filter(Boolean) : [],
  ) as GameObject[];
  if (objectTargets.length !== targets.filter((target) => typeof target.Object === "number").length) {
    return "target";
  }
  if (objectTargets.length === 0) return "target";
  if (objectTargets.every((obj) => !obj.card_types.core_types.includes("Land"))) {
    return "nonland permanent";
  }
  if (objectTargets.every((obj) => obj.card_types.core_types.includes("Creature"))) {
    return "creature";
  }
  if (objectTargets.every((obj) =>
    obj.card_types.core_types.includes("Planeswalker"),
  )) {
    return "planeswalker";
  }
  return "target permanent";
}
