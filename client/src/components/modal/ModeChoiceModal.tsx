import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useState } from "react";

import type { ModalChoice } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";

export function ModeChoiceModal() {
  const playerId = usePlayerId();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const [selected, setSelected] = useState<Set<number>>(new Set());

  const isModeChoice = waitingFor?.type === "ModeChoice";
  const modal: ModalChoice | null = isModeChoice ? waitingFor.data.modal : null;
  const isMyChoice = isModeChoice && waitingFor.data.player === playerId;

  const toggleMode = useCallback(
    (index: number) => {
      setSelected((prev) => {
        const next = new Set(prev);
        if (next.has(index)) {
          next.delete(index);
        } else if (modal && next.size < modal.max_choices) {
          next.add(index);
        }
        return next;
      });
    },
    [modal],
  );

  const handleConfirm = useCallback(() => {
    if (!modal) return;
    const indices = Array.from(selected).sort((a, b) => a - b);
    if (indices.length < modal.min_choices || indices.length > modal.max_choices) return;
    dispatch({ type: "SelectModes", data: { indices } });
    setSelected(new Set());
  }, [modal, selected, dispatch]);

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
    setSelected(new Set());
  }, [dispatch]);

  if (!isModeChoice || !isMyChoice || !modal) return null;

  const canConfirm = selected.size >= modal.min_choices && selected.size <= modal.max_choices;
  const isSingleChoice = modal.min_choices === 1 && modal.max_choices === 1;

  const chooseLabel =
    modal.min_choices === modal.max_choices
      ? `Choose ${numberWord(modal.min_choices)}`
      : `Choose ${numberWord(modal.min_choices)} to ${numberWord(modal.max_choices)}`;

  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-0 z-50 flex items-center justify-center"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.2 }}
      >
        <div className="absolute inset-0 bg-black/60" />

        <motion.div
          className="relative z-10 w-full max-w-md rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700"
          initial={{ scale: 0.9, opacity: 0 }}
          animate={{ scale: 1, opacity: 1 }}
          exit={{ scale: 0.9, opacity: 0 }}
          transition={{ duration: 0.2, ease: "easeOut" }}
        >
          <h2 className="mb-4 text-center text-lg font-bold text-white">
            {chooseLabel}
          </h2>

          <div className="flex flex-col gap-2">
            {modal.mode_descriptions.map((desc, index) => {
              const isSelected = selected.has(index);
              return (
                <button
                  key={index}
                  onClick={() => {
                    if (isSingleChoice) {
                      dispatch({ type: "SelectModes", data: { indices: [index] } });
                      setSelected(new Set());
                    } else {
                      toggleMode(index);
                    }
                  }}
                  className={`rounded-lg px-4 py-3 text-left transition ${
                    isSelected
                      ? "bg-cyan-900/50 ring-1 ring-cyan-400"
                      : "bg-gray-800 hover:bg-gray-700 hover:ring-1 hover:ring-cyan-400/50"
                  }`}
                >
                  <span className="font-semibold text-white">{desc}</span>
                </button>
              );
            })}
          </div>

          <div className="mt-4 flex justify-center gap-3">
            {!isSingleChoice && (
              <button
                onClick={handleConfirm}
                disabled={!canConfirm}
                className={`rounded-lg px-6 py-2 font-semibold shadow-lg transition ${
                  canConfirm
                    ? "bg-cyan-600 text-white hover:bg-cyan-500"
                    : "cursor-not-allowed bg-gray-700 text-gray-500"
                }`}
              >
                Confirm ({selected.size}/{modal.max_choices})
              </button>
            )}
            <button
              onClick={handleCancel}
              className="rounded-lg bg-gray-700 px-6 py-2 font-semibold text-gray-200 shadow-lg transition hover:bg-gray-600"
            >
              Cancel
            </button>
          </div>
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}

function numberWord(n: number): string {
  const words = ["zero", "one", "two", "three", "four", "five"];
  return words[n] ?? String(n);
}
