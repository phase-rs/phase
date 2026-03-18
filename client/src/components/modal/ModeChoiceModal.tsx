import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useState } from "react";

import type { ModalChoice } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";

export function ModeChoiceModal() {
  const playerId = usePlayerId();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const [selected, setSelected] = useState<number[]>([]);

  const isModeChoice = waitingFor?.type === "ModeChoice" || waitingFor?.type === "AbilityModeChoice";
  const isAbilityMode = waitingFor?.type === "AbilityModeChoice";
  const modal: ModalChoice | null = isModeChoice ? waitingFor.data.modal : null;
  const unavailableModes: number[] =
    isAbilityMode && "unavailable_modes" in waitingFor.data
      ? (waitingFor.data.unavailable_modes ?? [])
      : [];
  const isMyChoice = isModeChoice && waitingFor.data.player === playerId;

  const toggleMode = useCallback(
    (index: number) => {
      if (unavailableModes.includes(index)) return;
      setSelected((prev) => {
        if (!modal) return prev;

        if (modal.allow_repeat_modes) {
          if (prev.length >= modal.max_choices) {
            return prev;
          }
          return [...prev, index].sort((a, b) => a - b);
        }

        if (prev.includes(index)) {
          return prev.filter((value) => value !== index);
        }
        if (prev.length >= modal.max_choices) {
          return prev;
        }
        return [...prev, index].sort((a, b) => a - b);
      });
    },
    [modal, unavailableModes],
  );

  const handleConfirm = useCallback(() => {
    if (!modal) return;
    const indices = [...selected].sort((a, b) => a - b);
    if (indices.length < modal.min_choices || indices.length > modal.max_choices) return;
    dispatch({ type: "SelectModes", data: { indices } });
    setSelected([]);
  }, [modal, selected, dispatch]);

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
    setSelected([]);
  }, [dispatch]);

  if (!isModeChoice || !isMyChoice || !modal) return null;

  const canConfirm = selected.length >= modal.min_choices && selected.length <= modal.max_choices;
  const isSingleChoice = modal.min_choices === 1 && modal.max_choices === 1;

  const chooseLabel =
    modal.min_choices === modal.max_choices
      ? `Choose ${numberWord(modal.min_choices)}`
      : `Choose ${numberWord(modal.min_choices)} to ${numberWord(modal.max_choices)}`;

  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-0 z-50 flex items-center justify-center px-3 py-4 sm:px-4 sm:py-6"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.2 }}
      >
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
              {isAbilityMode ? "Ability Modes" : "Spell Modes"}
            </div>
            <h2 className="mt-1 text-lg font-semibold text-white sm:text-xl">{chooseLabel}</h2>
            <p className="mt-1 text-xs text-slate-400 sm:text-sm">
              Select the mode or modes to apply.
            </p>
          </div>
          <div className="px-4 py-4 sm:px-5 sm:py-5">
            <div className="flex flex-col gap-2">
              {modal.mode_descriptions.map((desc, index) => {
                const count = selected.filter((value) => value === index).length;
                const isSelected = count > 0;
                const isUnavailable = unavailableModes.includes(index);
                return (
                  <button
                    key={index}
                    disabled={isUnavailable}
                    onClick={() => {
                      if (isUnavailable) return;
                      if (isSingleChoice) {
                        dispatch({ type: "SelectModes", data: { indices: [index] } });
                        setSelected([]);
                      } else {
                        toggleMode(index);
                      }
                    }}
                    className={`rounded-[16px] border px-4 py-3 text-left transition ${
                      isUnavailable
                        ? "cursor-not-allowed border-white/5 bg-white/3 opacity-40"
                        : isSelected
                          ? "border-cyan-300/60 bg-cyan-500/12 ring-1 ring-cyan-400/40"
                          : "border-white/8 bg-white/5 hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
                    }`}
                  >
                    <span className={`font-semibold ${isUnavailable ? "text-slate-500" : "text-white"}`}>{desc}</span>
                    {isUnavailable && (
                      <span className="ml-2 text-xs text-slate-500">(already chosen)</span>
                    )}
                    {count > 0 && (
                      <span className="ml-2 inline-flex min-w-6 items-center justify-center rounded-full bg-cyan-300/20 px-2 py-0.5 text-xs font-semibold text-cyan-100">
                        {count}
                      </span>
                    )}
                  </button>
                );
              })}
            </div>

            <div className="mt-4 flex flex-col gap-3 sm:flex-row sm:justify-end">
              {!isSingleChoice && (
                <button
                  onClick={handleConfirm}
                  disabled={!canConfirm}
                  className={`min-h-11 rounded-[16px] px-6 py-2 font-semibold transition ${
                    canConfirm
                      ? "bg-cyan-500 text-slate-950 shadow-[0_14px_34px_rgba(6,182,212,0.28)] hover:bg-cyan-400"
                      : "cursor-not-allowed border border-white/8 bg-white/5 text-slate-500"
                  }`}
                >
                  Confirm ({selected.length}/{modal.max_choices})
                </button>
              )}
              {!isSingleChoice && selected.length > 0 && (
                <button
                  onClick={() => setSelected([])}
                  className="min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-6 py-2 font-semibold text-slate-200 transition hover:bg-white/8"
                >
                  Clear
                </button>
              )}
              {!isAbilityMode && (
                <button
                  onClick={handleCancel}
                  className="min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-6 py-2 font-semibold text-slate-200 transition hover:bg-white/8"
                >
                  Cancel
                </button>
              )}
            </div>
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
