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

        <motion.div
          className="relative z-10 max-h-[calc(100vh_-_2rem)] w-full max-w-md overflow-y-auto rounded-[24px] border border-white/10 bg-[#0b1020]/96 shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md"
          initial={{ scale: 0.95, opacity: 0, y: 10 }}
          animate={{ scale: 1, opacity: 1, y: 0 }}
          exit={{ scale: 0.95, opacity: 0, y: 10 }}
          transition={{ duration: 0.2, ease: "easeOut" }}
        >
          <div className="border-b border-white/10 px-4 py-4 sm:px-5 sm:py-5">
            <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
              Resolution Order
            </div>
            <h2 className="mt-1 text-lg font-semibold text-white sm:text-xl">
              Replacement Effects
            </h2>
            <p className="mt-1 text-xs text-slate-400 sm:text-sm">
              Choose which replacement effect applies first.
            </p>
          </div>
          <div className="px-4 py-4 sm:px-5 sm:py-5">
            <div className="flex flex-col gap-2">
              {candidates.map((index) => {
                const desc = candidateDescriptions[index];
                return (
                  <button
                    key={index}
                    onClick={() => handleChoose(index)}
                    className="min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/40"
                  >
                    <span className="font-semibold text-white">
                      {desc || `Replacement Effect ${index + 1}`}
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}
