import { AnimatePresence, motion } from "framer-motion";
import { useCallback } from "react";

import { useGameStore } from "../../stores/gameStore.ts";

export function ReplacementModal() {
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  const isReplacementChoice = waitingFor?.type === "ReplacementChoice";
  const candidateCount = isReplacementChoice
    ? waitingFor.data.candidate_count
    : 0;
  const candidateDescriptions: string[] = isReplacementChoice
    ? (waitingFor.data.candidate_descriptions ?? [])
    : [];

  const handleChoose = useCallback(
    (index: number) => {
      dispatch({ type: "ChooseReplacement", data: { index } });
    },
    [dispatch],
  );

  if (!isReplacementChoice || candidateCount === 0) return null;

  const candidates = Array.from({ length: candidateCount }, (_, i) => i);

  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-0 z-50 flex items-center justify-center px-3 py-4 sm:px-4 sm:py-6"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.2 }}
      >
        {/* Backdrop */}
        <div className="absolute inset-0 bg-black/60" />

        {/* Modal */}
        <motion.div
          className="relative z-10 max-h-[calc(100vh-2rem)] w-full max-w-md overflow-y-auto rounded-[20px] bg-gray-900 p-4 shadow-2xl ring-1 ring-gray-700 sm:p-6"
          initial={{ scale: 0.9, opacity: 0 }}
          animate={{ scale: 1, opacity: 1 }}
          exit={{ scale: 0.9, opacity: 0 }}
          transition={{ duration: 0.2, ease: "easeOut" }}
        >
          <h2 className="mb-2 text-center text-base font-bold text-white sm:text-lg">
            Replacement Effects
          </h2>
          <p className="mb-4 text-center text-sm text-gray-400">
            Choose which replacement effect applies first
          </p>

          <div className="flex flex-col gap-2">
            {candidates.map((index) => {
              const desc = candidateDescriptions[index];
              return (
                <button
                  key={index}
                  onClick={() => handleChoose(index)}
                  className="min-h-11 rounded-lg bg-gray-800 px-4 py-3 text-left transition hover:bg-gray-700 hover:ring-1 hover:ring-cyan-400/50"
                >
                  <span className="font-semibold text-white">
                    {desc || `Replacement Effect ${index + 1}`}
                  </span>
                </button>
              );
            })}
          </div>
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}
