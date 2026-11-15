import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { AmountInput, parseAmount } from "./AmountInput.tsx";

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
  // Prompt identity, not just bounds: a successor prompt with the SAME max would otherwise leave
  // `raw` holding the previous decision, letting the player submit an amount they chose for a
  // different prompt. BOTH seats are keyed on, not just `caster` — `chosen` is the seat actually
  // being asked to pay, so one caster polling two different players in succession changes only
  // `chosen`, and keying on `caster` alone would not fire.
  const caster = isAssistPayment ? waitingFor.data.caster : null;
  const chosen = isAssistPayment ? waitingFor.data.chosen : null;
  const [raw, setRaw] = useState("0");

  useEffect(() => {
    if (isAssistPayment) setRaw("0");
  }, [isAssistPayment, max, caster, chosen]);

  // CR 702.132a: "the player you chose may pay for any amount of the generic mana in the
  // spell's total cost" — the variant's domain is 0..max_generic. This is NOT a
  // frontend-invented bound: `WaitingFor::AssistPayment` carries no `min` field, and the
  // engine's own prompt projection (`game::interaction::number_projection`) synthesizes
  // `min: 0` for this variant. The client mirrors the engine's projection.
  const amount = parseAmount(raw, 0, max);

  const handleCommit = useCallback(() => {
    if (amount === null) return;
    dispatch({ type: "CommitAssistPayment", data: { generic: amount } });
  }, [dispatch, amount]);

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

          {/* Reusing `assist.payment.title` as the box's accessible name preserves today's
              accessible name byte-for-byte (the slider already carried it) and adds no key. */}
          <AmountInput
            raw={raw}
            onRawChange={setRaw}
            min={0}
            max={max}
            onSubmit={handleCommit}
            labels={{
              input: t("assist.payment.title"),
              decrease: t("mana.decreaseAmount"),
              increase: t("mana.increaseAmount"),
            }}
          />

          <div className="flex justify-center">
            <button
              onClick={handleCommit}
              disabled={amount === null}
              className={gameButtonClass({
                tone: "emerald",
                size: "md",
                disabled: amount === null,
              })}
            >
              {/* `(amount ?? 0) === 0` collapsed "invalid entry" into "declining to pay", so an
                  over-max entry labelled the button "Pay nothing" — the OPPOSITE of the pending
                  intent. Declining is amount === 0 exactly; null is no decision yet. */}
              {amount === null
                ? t("mana.confirmAmount")
                : amount === 0
                  ? t("assist.payment.decline")
                  : t("assist.payment.commit", { value: amount })}
            </button>
          </div>
        </div>
      </motion.div>
    </AnimatePresence>
  );
}
