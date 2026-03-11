import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useEffect, useRef } from "react";

import type { ObjectId, TargetRef } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { TargetArrow } from "./TargetArrow.tsx";

export function TargetingOverlay() {
  const playerId = usePlayerId();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const gameState = useGameStore((s) => s.gameState);
  const dispatch = useGameStore((s) => s.dispatch);
  const targetingMode = useUiStore((s) => s.targetingMode);
  const selectedTargets = useUiStore((s) => s.selectedTargets);
  const sourceObjectId = useUiStore((s) => s.sourceObjectId);
  const startTargeting = useUiStore((s) => s.startTargeting);
  const clearTargets = useUiStore((s) => s.clearTargets);

  const sourceRef = useRef<{ x: number; y: number } | null>(null);

  const isTargetSelection = waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection";
  const pendingCast = waitingFor?.type === "TargetSelection" ? waitingFor.data.pending_cast : null;
  const legalTargets = isTargetSelection ? waitingFor!.data.legal_targets : null;
  const isTriggerTargeting = waitingFor?.type === "TriggerTargetSelection";

  // Activate targeting mode when engine requests target selection
  useEffect(() => {
    if (!isTargetSelection || !gameState || !legalTargets) return;

    // Extract object IDs from engine's legal targets (filter out Player refs)
    const validIds = legalTargets
      .filter((t): t is { Object: ObjectId } => "Object" in t)
      .map((t) => t.Object);
    const sourceId = pendingCast?.object_id ?? null;

    startTargeting(validIds, sourceId);

    return () => {
      clearTargets();
    };
  }, [isTargetSelection, gameState, legalTargets, pendingCast, startTargeting, clearTargets]);

  // Track source element position for arrow drawing
  useEffect(() => {
    if (!sourceObjectId) {
      sourceRef.current = null;
      return;
    }
    const el = document.querySelector(`[data-object-id="${sourceObjectId}"]`);
    if (el) {
      const rect = el.getBoundingClientRect();
      sourceRef.current = { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
    }
  }, [sourceObjectId]);

  const handleConfirm = useCallback(() => {
    const targets: TargetRef[] = selectedTargets.map((id) => ({ Object: id }));
    dispatch({ type: "SelectTargets", data: { targets } });
    clearTargets();
  }, [selectedTargets, dispatch, clearTargets]);

  const handleCancel = useCallback(() => {
    clearTargets();
    dispatch({ type: "CancelCast" });
  }, [clearTargets, dispatch]);

  // Get target element positions for arrows
  const getTargetPos = (objectId: ObjectId) => {
    const el = document.querySelector(`[data-object-id="${objectId}"]`);
    if (!el) return null;
    const rect = el.getBoundingClientRect();
    return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  };

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

        {/* Action buttons (must be clickable) */}
        <div className="pointer-events-auto absolute bottom-6 left-0 right-0 flex justify-center gap-4">
          {selectedTargets.length > 0 && (
            <button
              onClick={handleConfirm}
              className="rounded-lg bg-cyan-600 px-6 py-2 font-semibold text-white shadow-lg transition hover:bg-cyan-500"
            >
              Confirm Target
            </button>
          )}
          {!isTriggerTargeting && (
            <button
              onClick={handleCancel}
              className="rounded-lg bg-gray-700 px-6 py-2 font-semibold text-gray-200 shadow-lg transition hover:bg-gray-600"
            >
              Cancel
            </button>
          )}
        </div>

        {/* Arrows from source to selected targets */}
        {sourceRef.current &&
          selectedTargets.map((targetId) => {
            const targetPos = getTargetPos(targetId);
            if (!targetPos || !sourceRef.current) return null;
            return (
              <TargetArrow
                key={targetId}
                from={sourceRef.current}
                to={targetPos}
              />
            );
          })}
      </motion.div>
    </AnimatePresence>
  );
}
