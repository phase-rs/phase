import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { manaCostToShards } from "../../viewmodel/costLabel.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { AmountInput, parseAmount } from "./AmountInput.tsx";
import { ManaSymbol } from "./ManaSymbol.tsx";

/**
 * Overlay for the `WaitingFor::ChooseXValue` state.
 *
 * CR 107.3a + CR 601.2b: the controller of the spell chooses and announces X as part of
 * casting it; CR 601.2f then locks in the total cost that X feeds, so the value must be
 * settled before mana is paid. CR 107.1b (a negative number can't be chosen) is why the
 * lower bound is never below 0. The engine computes the upper bound (`max`) from the
 * player's pool + untapped free-to-tap producers; this component is a pure
 * display layer that dispatches the caster's chosen value via `ChooseX`.
 */
export function ChooseXValueUI() {
  const { t } = useTranslation("game");
  const waitingFor = useGameStore((s) => s.waitingFor);
  const gameState = useGameStore((s) => s.gameState);
  const dispatch = useGameStore((s) => s.dispatch);
  const canAct = useCanActForWaitingState();

  const isChooseX = waitingFor?.type === "ChooseXValue";
  const min = isChooseX ? (waitingFor.data.min ?? 0) : 0;
  const max = isChooseX ? waitingFor.data.max : 0;
  const hasValidBounds = min <= max;
  const defaultValue = hasValidBounds ? Math.max(min, 0) : 0;
  const pendingCast = isChooseX ? waitingFor.data.pending_cast : null;
  const xCostPreviews = isChooseX ? waitingFor.data.x_cost_previews : undefined;

  const [raw, setRaw] = useState(String(defaultValue));
  const amount = parseAmount(raw, min, max);

  const pendingCostShards = useMemo(() => {
    if (!pendingCast) return null;
    // While the entry is invalid there is no chosen X, so there is no cost to preview. Falling
    // back to `min` here would render the mana cost of an X the caster never typed — the same
    // defect on the display side that the commit guard below fixes on the dispatch side.
    if (amount === null) return null;
    const previewCost = xCostPreviews?.find(([x]) => x === amount)?.[1];
    const cost = previewCost ?? pendingCast.cost;
    const shards = manaCostToShards(cost);
    return shards.length > 0 ? shards : null;
  }, [pendingCast, xCostPreviews, amount]);

  const cardName = useMemo(() => {
    if (!gameState || !pendingCast) return null;
    return gameState.objects[pendingCast.object_id]?.name ?? null;
  }, [gameState, pendingCast]);

  useEffect(() => {
    if (isChooseX) setRaw(String(defaultValue));
    // `pendingCast?.object_id` keys the reset on prompt IDENTITY, not just its window: a
    // successor ChooseXValue for a DIFFERENT spell with the same [min, max] would otherwise
    // leave `raw` holding the X chosen for the previous spell.
    // `max` is a dependency even though it does not appear in the body: re-entering ChooseXValue
    // with a NARROWER max but an unchanged min leaves `defaultValue` identical, so without it the
    // effect would not fire and a now-out-of-range entry would persist with no way to self-heal.
    // Both sibling prompts already key their reset on their full window plus their own identity
    // fields; this closes that asymmetry. (An earlier version enumerated those arrays here — the
    // same commit that added the seat deps made the enumeration wrong, so it names the shape
    // instead of the members.)
  }, [isChooseX, defaultValue, max, pendingCast?.object_id]);

  const handleCommit = useCallback(() => {
    // Sanitization gate: an out-of-range X is REJECTED, not clamped. Typing 99 under max=5
    // used to silently cast for 5 — a value the caster never chose. CR 107.3a: the controller
    // "chooses and announces the value of X"; CR 601.2f governs total cost, not that choice.
    if (amount === null) return;
    dispatch({
      type: "ChooseX",
      data: { value: amount },
    });
  }, [amount, dispatch]);

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
  }, [dispatch]);

  // CR 107.3a: X is chosen and announced by the spell's controller, so only that player gets
  // this panel; opponents observe via the stack ghost entry, not an interactive panel.
  if (!isChooseX || !canAct || !hasValidBounds) return null;

  return (
    <AnimatePresence>
      <motion.div
        className="pointer-events-none fixed inset-x-0 bottom-0 z-40 flex justify-center pb-4"
        initial={{ y: 80, opacity: 0 }}
        animate={{ y: 0, opacity: 1 }}
        exit={{ y: 80, opacity: 0 }}
        transition={{ duration: 0.25 }}
      >
        {/* Re-enable events on the panel: when DialogHost is click-through
            (peeked convoke payment) or `pointer-events: none`, the strip must
            opt back in — same contract as ManaPaymentUI (CR 702.51a). */}
        <div className="pointer-events-auto rounded-xl bg-gray-900/95 p-4 shadow-2xl ring-1 ring-gray-700 min-w-[320px] max-w-[420px]">
          <h3 className="mb-3 text-center text-sm font-semibold text-gray-300">
            {t("mana.chooseXTitle")}
            {cardName && (
              <span className="ml-1 text-gray-400">&mdash; {cardName}</span>
            )}
          </h3>

          {pendingCostShards && (
            <div className="mb-3 flex items-center justify-center gap-1.5">
              {pendingCostShards.map((shard, idx) => (
                <ManaSymbol key={idx} shard={shard} size="lg" />
              ))}
            </div>
          )}

          <AmountInput
            raw={raw}
            onRawChange={setRaw}
            min={min}
            max={max}
            onSubmit={handleCommit}
            labels={{
              input: t("mana.chooseXInputAria"),
              decrease: t("mana.decreaseX"),
              increase: t("mana.increaseX"),
              setMaximum: t("mana.setXToMaximum"),
            }}
          />

          <div className="flex justify-center gap-3">
            <button
              onClick={handleCommit}
              disabled={amount === null}
              className={gameButtonClass({
                tone: "emerald",
                size: "md",
                disabled: amount === null,
              })}
            >
              {/* No chosen X yet ⇒ name the action without a value. `amount ?? min` here would
                  label the button "Confirm X = <min>" while the player has 99 typed. */}
              {amount === null
                ? t("mana.confirmAmount")
                : t("mana.confirmX", { value: amount })}
            </button>
            <button
              onClick={handleCancel}
              className="rounded-lg bg-gray-700 px-4 py-1.5 text-sm font-semibold text-gray-200 transition hover:bg-gray-600"
            >
              {t("common:actions.cancel")}
            </button>
          </div>
        </div>
      </motion.div>
    </AnimatePresence>
  );
}
