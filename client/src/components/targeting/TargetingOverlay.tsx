import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useEffect } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { renderDescription } from "../../utils/description.ts";
import type { GameObject } from "../../adapter/types.ts";
import { RichLabel } from "../mana/RichLabel.tsx";

export function TargetingOverlay() {
  const { t } = useTranslation("game");
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const objects = useGameStore((s) => s.gameState?.objects);
  const stack = useGameStore((s) => s.gameState?.stack);
  const selectedCardIds = useUiStore((s) => s.selectedCardIds);
  const clearSelectedCards = useUiStore((s) => s.clearSelectedCards);

  const isTargetSelection = waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection";
  const isCopyTargetChoice = waitingFor?.type === "CopyTargetChoice";
  const isCopyRetarget = waitingFor?.type === "CopyRetarget";
  const canKeepCurrentTargets = isCopyRetarget && waitingFor.data.target_slots.every((slot) => slot.current != null);
  const isExploreChoice = waitingFor?.type === "ExploreChoice";
  // CR 701.36a: Populate — choose a creature token you control to copy.
  const isPopulateChoice = waitingFor?.type === "PopulateChoice";
  // CR 303.4 + CR 303.4g + CR 115.1: Return-as-Aura attach pick. Picker is a
  // CHOICE (not a target), but the action shape mirrors ExploreChoice
  // (`GameAction::ChooseTarget` with the chosen ObjectId).
  const isReturnAsAuraTarget = waitingFor?.type === "ReturnAsAuraTarget";
  // CR 115.7: Single-target retargets (Bolt Bend, Redirect) are picked on the
  // board through this overlay; multi-target retargets keep the dialog.
  const isRetargetChoice = waitingFor?.type === "RetargetChoice" && waitingFor.data.scope.type === "Single";
  // CR 115.7: Name the spell/ability being retargeted (the entry the redirect
  // resolved onto), so the player knows what they are choosing a new target for.
  const retargetSpellName = isRetargetChoice
    ? objects?.[stack?.[waitingFor.data.stack_entry_index]?.source_id ?? -1]?.name
    : undefined;
  const isTapCreatureChoice =
    waitingFor?.type === "PayCost" && waitingFor.data.kind.type === "TapCreatures";
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
      : waitingFor?.type === "PopulateChoice"
        ? waitingFor.data.source_id
      : waitingFor?.type === "ReturnAsAuraTarget"
        ? waitingFor.data.source_id
      : waitingFor?.type === "PayCost" && waitingFor.data.kind.type === "TapCreatures"
        ? waitingFor.data.resume.type === "ManaAbility"
          ? (waitingFor.data.resume.ManaAbility as { source_id?: number } | undefined)?.source_id
          : (waitingFor.data.resume.Spell as { object_id?: number } | undefined)?.object_id
      : undefined;
  const sourceName = sourceId != null ? objects?.[sourceId]?.name : undefined;

  const inferredPrompt = buildInferredTargetPrompt({
    waitingFor: isTargetSelection ? waitingFor : null,
    objects,
    activeSlot,
    targetSlots,
    selection,
    t,
  });

  const triggerDescription = waitingFor?.type === "TriggerTargetSelection" && waitingFor.data.description
    ? renderDescription(waitingFor.data.description, sourceName ?? "this")
    : undefined;
  const spellTargetDescription = waitingFor?.type === "TargetSelection" && waitingFor.data.pending_cast.ability.description
    ? renderDescription(waitingFor.data.pending_cast.ability.description, sourceName ?? "this")
    : undefined;
  const enginePrompt = triggerDescription ?? spellTargetDescription;

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

  if (!isTargetSelection && !isCopyTargetChoice && !isCopyRetarget && !isExploreChoice && !isPopulateChoice && !isReturnAsAuraTarget && !isRetargetChoice && !isTapCreatureChoice) return null;

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
              ? t("targeting.choosePermanentToCopy")
              : isCopyRetarget
                ? (() => {
                    const slots = waitingFor.data.target_slots;
                    const hasCurrent = slots.every((slot) => slot.current != null);
                    return slots.length > 1
                      ? (hasCurrent
                          ? t("targeting.retargetCopySlot", { current: Math.min(currentTargetSlot + 1, slots.length), total: slots.length })
                          : t("targeting.chooseTargetForCopySlot", { current: Math.min(currentTargetSlot + 1, slots.length), total: slots.length }))
                      : hasCurrent ? t("targeting.chooseNewTargetForCopy") : t("targeting.chooseTargetForCopy");
                  })()
              : isExploreChoice
                ? t("targeting.chooseCreatureToExplore")
              : isPopulateChoice
                ? t("targeting.chooseCreatureTokenToPopulate")
              : isReturnAsAuraTarget
                ? t("targeting.chooseReturnAsAuraTarget")
              : isRetargetChoice
                ? (retargetSpellName
                    ? t("targeting.chooseNewTargetForSpell", { spell: retargetSpellName })
                    : t("targeting.chooseNewTarget"))
              : isTapCreatureChoice
                ? t("targeting.tapUntappedCreatures", { count: waitingFor.data.count })
              : inferredPrompt ?? (
                targetSlots.length > 1
                  ? t("targeting.chooseTargetOf", { current: Math.min(currentTargetSlot + 1, targetSlots.length), total: targetSlots.length })
                  : t("targeting.chooseTarget")
              )}
            </div>
          {enginePrompt && (
            <div className="max-w-md rounded-md bg-gray-800/90 px-4 py-1 text-center text-xs text-gray-300 shadow">
              <RichLabel text={enginePrompt} size="xs" />
            </div>
          )}
        </div>

        {/* Player targets are handled by PlayerHud/OpponentHud glow + click */}

        <div className="pointer-events-auto absolute bottom-6 left-0 right-0 flex justify-center gap-4">
          {(waitingFor.type === "TargetSelection" ||
            (waitingFor.type === "PayCost" &&
              waitingFor.data.kind.type === "TapCreatures" &&
              waitingFor.data.resume.type === "Spell")) && (
            <button
              onClick={handleCancel}
              className="rounded-lg bg-gray-700 px-6 py-2 font-semibold text-gray-200 shadow-lg transition hover:bg-gray-600"
            >
              {t("common:actions.cancel")}
            </button>
          )}
          {isTapCreatureChoice && (
            <button
              onClick={handleConfirmTap}
              disabled={selectedCardIds.length !== waitingFor.data.count}
              className="rounded-lg bg-emerald-700 px-6 py-2 font-semibold text-gray-100 shadow-lg transition hover:bg-emerald-600 disabled:cursor-not-allowed disabled:bg-gray-700 disabled:text-gray-400"
            >
              {t("targeting.confirmTap", { selected: selectedCardIds.length, count: waitingFor.data.count })}
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
              {t("targeting.keepCurrentTargets")}
            </button>
          )}
          {isOptionalCurrentSlot && (
            <button
              onClick={handleSkip}
              className="rounded-lg bg-amber-700 px-6 py-2 font-semibold text-gray-100 shadow-lg transition hover:bg-amber-600"
            >
              {t("targeting.skip")}
            </button>
          )}
        </div>
      </motion.div>
    </AnimatePresence>
  );
}

type TargetingPromptParams = {
  waitingFor: {
    type: "TargetSelection" | "TriggerTargetSelection" | "ExploreChoice" | "CopyTargetChoice" | "PayCost";
    data: {
      target_slots?: { legal_targets: { Object?: number; Player?: number }[]; optional?: boolean }[];
      mode_labels?: (string | null)[];
      selection?: { current_slot: number };
      player?: number;
    };
  } | null;
  objects?: Record<number, GameObject> | undefined;
  activeSlot: { legal_targets: { Object?: number; Player?: number }[]; optional?: boolean } | undefined;
  targetSlots: { legal_targets: { Object?: number; Player?: number }[]; optional?: boolean }[];
  selection: { current_slot: number } | null;
  t: TFunction<"game">;
};

function buildInferredTargetPrompt({
  waitingFor,
  objects,
  activeSlot,
  targetSlots,
  selection,
  t,
}: TargetingPromptParams): string | null {
  if (!waitingFor) return null;
  if (waitingFor.type !== "TargetSelection" && waitingFor.type !== "TriggerTargetSelection") return null;
  if (!selection) return null;

  if (!activeSlot) return null;
  // Skip mode-context wrapping on the no-legal-targets branch (CR 700.2c): the
  // engine does not surface a target for this slot, so there is no mode prompt
  // to qualify.
  if (activeSlot.legal_targets.length === 0) {
    return t("targeting.noLegalTargets");
  }

  const targetWord = inferTargetNoun(activeSlot.legal_targets, objects, t);
  const useUpToOne = selection && targetSlots.length === 1 && activeSlot.optional;

  let prompt: string;
  if (waitingFor.type === "TriggerTargetSelection") {
    prompt = useUpToOne ? t("targeting.upToOne", { target: targetWord }) : t("targeting.one", { target: targetWord });
  } else if (targetSlots.length <= 1) {
    prompt = useUpToOne ? t("targeting.upToOne", { target: targetWord }) : t("targeting.one", { target: targetWord });
  } else {
    prompt = t("targeting.chooseTargetOf", { current: Math.min(selection.current_slot + 1, targetSlots.length), total: targetSlots.length });
  }

  // CR 700.2 / CR 601.2b: For a modal spell/ability, the engine attaches a
  // per-slot mode label so the player knows which chosen mode the current
  // target belongs to. Wrap the computed prompt once with the active slot's
  // label when present. `mode = ` interpolates the raw engine label (Oracle
  // mode text, not localized); `prompt` is the already-localized base.
  const modeLabel = waitingFor.data.mode_labels?.[selection.current_slot];
  if (modeLabel) {
    return t("targeting.modeContext", { mode: modeLabel, prompt });
  }
  return prompt;
}

function inferTargetNoun(
  targets: { Object?: number; Player?: number }[],
  objects: Record<number, GameObject> | undefined,
  t: TFunction<"game">,
): string {
  const allPlayers = targets.every((target) => "Player" in target);
  if (allPlayers) return t("targeting.nounPlayer");

  const objectTargets = targets.flatMap((target) =>
    typeof target.Object === "number" ? [objects?.[target.Object]].filter(Boolean) : [],
  ) as GameObject[];
  if (objectTargets.length !== targets.filter((target) => typeof target.Object === "number").length) {
    return t("targeting.nounTarget");
  }
  if (objectTargets.length === 0) return t("targeting.nounTarget");
  if (objectTargets.every((obj) => !obj.card_types.core_types.includes("Land"))) {
    return t("targeting.nounNonlandPermanent");
  }
  if (objectTargets.every((obj) => obj.card_types.core_types.includes("Creature"))) {
    return t("targeting.nounCreature");
  }
  if (objectTargets.every((obj) =>
    obj.card_types.core_types.includes("Planeswalker"),
  )) {
    return t("targeting.nounPlaneswalker");
  }
  return t("targeting.nounTargetPermanent");
}
