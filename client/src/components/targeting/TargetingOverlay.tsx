import { AnimatePresence, motion } from "framer-motion";
import { useCallback } from "react";

import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";

export function TargetingOverlay() {
  const playerId = usePlayerId();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  const isTargetSelection = waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection";
  const isCopyTargetChoice = waitingFor?.type === "CopyTargetChoice";
  const targetSlots = isTargetSelection ? waitingFor.data.target_slots : [];
  const selection = isTargetSelection ? waitingFor.data.selection : null;
  const currentTargetSlot = selection?.current_slot ?? 0;
  const activeSlot = targetSlots[currentTargetSlot];
  const isOptionalCurrentSlot = activeSlot?.optional === true;

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
  }, [dispatch]);

  const handleSkip = useCallback(() => {
    dispatch({ type: "ChooseTarget", data: { target: null } });
  }, [dispatch]);

  if (!isTargetSelection && !isCopyTargetChoice) return null;

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
            {isCopyTargetChoice
              ? "Choose a permanent to copy"
              : targetSlots.length > 1
                ? `Choose target ${Math.min(currentTargetSlot + 1, targetSlots.length)} of ${targetSlots.length}`
                : "Choose a target"}
          </div>
        </div>

        {/* Player targets are handled by PlayerHud/OpponentHud glow + click */}

        <div className="pointer-events-auto absolute bottom-6 left-0 right-0 flex justify-center gap-4">
          {waitingFor.type === "TargetSelection" && (
            <button
              onClick={handleCancel}
              className="rounded-lg bg-gray-700 px-6 py-2 font-semibold text-gray-200 shadow-lg transition hover:bg-gray-600"
            >
              Cancel
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
