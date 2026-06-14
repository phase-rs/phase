import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";

/**
 * CR 702.132a: Assist — the chosen helper decides how much of the spell's
 * generic mana to pay (0..`max_generic`, 0 = contribute nothing). The engine
 * re-validates feasibility, so this overlay does no mana math; it only collects
 * the amount and dispatches `CommitAssistPayment`. The `chosen` seat is the
 * actor here (routed via `waitingPlayer`'s AssistPayment branch).
 */
export function AssistPaymentUI() {
  const { t } = useTranslation("game");
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const canAct = useCanActForWaitingState();

  const isAssistPayment = waitingFor?.type === "AssistPayment";
  const max = isAssistPayment ? waitingFor.data.max_generic : 0;
  const [value, setValue] = useState(0);

  useEffect(() => {
    if (isAssistPayment) setValue(0);
  }, [isAssistPayment, max]);

  const handleCommit = useCallback(() => {
    dispatch({ type: "CommitAssistPayment", data: { generic: value } });
  }, [dispatch, value]);

  if (!isAssistPayment || !canAct) return null;

  return (
    <AnimatePresence>
      <motion.div
        className="pointer-events-none fixed inset-x-0 bottom-0 z-40 flex justify-center pb-4"
        initial={{ y: 80, opacity: 0 }}
        animate={{ y: 0, opacity: 1 }}
        exit={{ y: 80, opacity: 0 }}
        transition={{ duration: 0.25 }}
      >
        <div className="pointer-events-auto min-w-[320px] max-w-[420px] rounded-xl bg-gray-900/95 p-4 shadow-2xl ring-1 ring-gray-700">
          <h3 className="mb-3 text-center text-sm font-semibold text-gray-300">
            {t("assist.payment.title")}
          </h3>

          <div className="mb-4 px-2">
            <label className="flex items-center gap-3 text-sm text-gray-200">
              <span className="shrink-0 font-mono text-base text-cyan-300">
                {value}
              </span>
              <input
                type="range"
                min={0}
                max={max}
                value={value}
                onChange={(e) => setValue(Number(e.target.value))}
                className="h-2 w-full cursor-pointer appearance-none rounded-full bg-gray-700 accent-cyan-500"
                aria-label={t("assist.payment.title")}
              />
              <span className="shrink-0 text-xs text-gray-500">/ {max}</span>
            </label>
          </div>

          <div className="flex justify-center">
            <button
              onClick={handleCommit}
              className={gameButtonClass({ tone: "emerald", size: "md" })}
            >
              {value === 0
                ? t("assist.payment.decline")
                : t("assist.payment.commit", { value })}
            </button>
          </div>
        </div>
      </motion.div>
    </AnimatePresence>
  );
}
