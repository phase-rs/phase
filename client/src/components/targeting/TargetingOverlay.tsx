import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useEffect } from "react";

import type { ObjectId } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";

export function TargetingOverlay() {
  const playerId = usePlayerId();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const gameState = useGameStore((s) => s.gameState);
  const dispatch = useGameStore((s) => s.dispatch);
  const targetingMode = useUiStore((s) => s.targetingMode);
  const startTargeting = useUiStore((s) => s.startTargeting);
  const clearTargets = useUiStore((s) => s.clearTargets);

  const isTargetSelection = waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection";
  const pendingCast = waitingFor?.type === "TargetSelection" ? waitingFor.data.pending_cast : null;
  const legalTargets = isTargetSelection ? waitingFor!.data.legal_targets : null;
  const isTriggerTargeting = waitingFor?.type === "TriggerTargetSelection";
  const isOptionalTrigger = isTriggerTargeting && waitingFor?.data?.optional === true;

  // Activate targeting mode when engine requests target selection
  useEffect(() => {
    if (!isTargetSelection || !gameState || !legalTargets) return;

    const validIds = legalTargets
      .filter((t): t is { Object: ObjectId } => "Object" in t)
      .map((t) => t.Object);
    const sourceId = pendingCast?.object_id ?? null;

    startTargeting(validIds, sourceId);

    return () => {
      clearTargets();
    };
  }, [isTargetSelection, gameState, legalTargets, pendingCast, startTargeting, clearTargets]);

  const handleCancel = useCallback(() => {
    clearTargets();
    dispatch({ type: "CancelCast" });
  }, [clearTargets, dispatch]);

  const handleDecline = useCallback(() => {
    clearTargets();
    dispatch({ type: "SelectTargets", targets: [] });
  }, [clearTargets, dispatch]);

  if (!targetingMode || !isTargetSelection) return null;

  // Only show targeting UI for the human player
  if (waitingFor.data.player !== playerId) return null;

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
        <div className="absolute left-0 right-0 top-4 flex justify-center">
          <div className="rounded-lg bg-gray-900/90 px-6 py-2 text-lg font-semibold text-cyan-400 shadow-lg">
            {isTriggerTargeting ? "Choose a target for triggered ability" : "Choose a target"}
          </div>
        </div>

        {/* Cancel button (voluntary casts) or Decline button (optional triggers) */}
        {!isTriggerTargeting && (
          <div className="pointer-events-auto absolute bottom-6 left-0 right-0 flex justify-center gap-4">
            <button
              onClick={handleCancel}
              className="rounded-lg bg-gray-700 px-6 py-2 font-semibold text-gray-200 shadow-lg transition hover:bg-gray-600"
            >
              Cancel
            </button>
          </div>
        )}
        {isOptionalTrigger && (
          <div className="pointer-events-auto absolute bottom-6 left-0 right-0 flex justify-center gap-4">
            <button
              onClick={handleDecline}
              className="rounded-lg bg-amber-700 px-6 py-2 font-semibold text-gray-100 shadow-lg transition hover:bg-amber-600"
            >
              Decline
            </button>
          </div>
        )}
      </motion.div>
    </AnimatePresence>
  );
}
