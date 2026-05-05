import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useEffect, useMemo, useState } from "react";

import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";

export function PayAmountChoiceUI() {
  const waitingFor = useGameStore((s) => s.waitingFor);
  const gameState = useGameStore((s) => s.gameState);
  const dispatch = useGameStore((s) => s.dispatch);
  const canAct = useCanActForWaitingState();

  const isPayAmount = waitingFor?.type === "PayAmountChoice";
  const data = isPayAmount ? waitingFor.data : null;
  const min = data?.min ?? 0;
  const max = data?.max ?? 0;
  const [value, setValue] = useState(min);

  useEffect(() => {
    if (isPayAmount) setValue(min);
  }, [isPayAmount, min, max]);

  const sourceName = useMemo(() => {
    if (!gameState || !data) return null;
    return gameState.objects[data.source_id]?.name ?? null;
  }, [gameState, data]);

  const resourceLabel = useMemo(() => {
    if (!data) return "amount";
    switch (data.resource.type) {
      case "Energy":
        return "energy";
      case "ManaGeneric":
        return "mana";
    }
  }, [data]);

  const handleCommit = useCallback(() => {
    dispatch({ type: "SubmitPayAmount", data: { amount: value } });
  }, [dispatch, value]);

  if (!data || !canAct) return null;

  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-x-0 bottom-0 z-40 flex justify-center pb-4"
        initial={{ y: 80, opacity: 0 }}
        animate={{ y: 0, opacity: 1 }}
        exit={{ y: 80, opacity: 0 }}
        transition={{ duration: 0.25 }}
      >
        <div className="min-w-[320px] max-w-[420px] rounded-xl bg-gray-900/95 p-4 shadow-2xl ring-1 ring-gray-700">
          <h3 className="mb-3 text-center text-sm font-semibold text-gray-300">
            Choose amount to pay
            {sourceName && (
              <span className="ml-1 text-gray-400">&mdash; {sourceName}</span>
            )}
          </h3>

          <div className="mb-4 px-2">
            <label className="flex items-center gap-3 text-sm text-gray-200">
              <span className="shrink-0 font-mono text-base text-cyan-300">
                X = {value}
              </span>
              <input
                type="range"
                min={min}
                max={max}
                value={value}
                onChange={(e) => setValue(Number(e.target.value))}
                className="h-2 w-full cursor-pointer appearance-none rounded-full bg-gray-700 accent-cyan-500"
                aria-label="Choose amount to pay"
              />
              <span className="shrink-0 text-xs text-gray-500">
                max {max}
              </span>
            </label>
          </div>

          <div className="flex justify-center">
            <button
              onClick={handleCommit}
              className={gameButtonClass({ tone: "emerald", size: "md" })}
            >
              Pay {value} {resourceLabel}
            </button>
          </div>
        </div>
      </motion.div>
    </AnimatePresence>
  );
}
