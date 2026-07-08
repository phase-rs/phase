import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";

export function PayAmountChoiceUI() {
  const { t } = useTranslation("game");
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
    if (!data) return "";
    switch (data.resource.type) {
      case "Energy":
        return t("mana.resourceEnergy");
      case "ManaGeneric":
        return t("mana.resourceMana");
      case "Counters":
        return t("mana.resourceCounters");
      case "Speed":
        return t("mana.resourceSpeed");
    }
  }, [data, t]);

  const handleCommit = useCallback(() => {
    dispatch({ type: "SubmitPayAmount", data: { amount: value } });
  }, [dispatch, value]);

  if (!data || !canAct) return null;

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
            {t("mana.chooseAmountTitle")}
            {sourceName && (
              <span className="ml-1 text-gray-400">&mdash; {sourceName}</span>
            )}
          </h3>

          <div className="mb-4 px-2">
            <label className="flex items-center gap-3 text-sm text-gray-200">
              <span className="shrink-0 font-mono text-base text-cyan-300">
                {t("mana.xEquals", { value })}
              </span>
              <input
                type="range"
                min={min}
                max={max}
                value={value}
                onChange={(e) => setValue(Number(e.target.value))}
                className="h-2 w-full cursor-pointer appearance-none rounded-full bg-gray-700 accent-cyan-500"
                aria-label={t("mana.chooseAmountAria")}
              />
              <span className="shrink-0 text-xs text-gray-500">
                {t("mana.maxOnly", { max })}
              </span>
            </label>
          </div>

          <div className="flex justify-center">
            <button
              onClick={handleCommit}
              className={gameButtonClass({ tone: "emerald", size: "md" })}
            >
              {t("mana.payAmount", { value, resource: resourceLabel })}
            </button>
          </div>
        </div>
      </motion.div>
    </AnimatePresence>
  );
}
