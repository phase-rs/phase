import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { AmountInput, parseAmount } from "./AmountInput.tsx";

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
  // Prompt identity, not just bounds — a successor prompt with the same window would otherwise
  // leave `raw` holding the amount chosen for the PREVIOUS prompt.
  // `player` as well as `source_id`: the engine emits consecutive PayAmountChoice states with a
  // constant `source_id` and a changing `player`. `effects/pay.rs` drives `PlayerFilter::All` and
  // its own test asserts prompt 1 `player=0` then prompt 2 `player=1` with no intervening
  // `WaitingFor` — but there `max` and `accumulated` move too, so that case alone does not show
  // `source_id` is insufficient.
  // The case that does is the LoopCollapse arm in `game/turns.rs`, which is the prompt this
  // component's own tests model: it mints one prompt per controller in APNAP order with
  // `min: 0`, `accumulated: 0`, `source_id: ObjectId(0)` and `pending_mana_ability: None` all
  // written as LITERALS — constant by construction, not conditional on board state. Only `max`
  // (the controller's pending count) and the `LoopCollapse` axis vary, so two controllers with
  // equal counts on the same axis produce successive prompts differing in `player` alone, and
  // keying on `source_id` would leave the second controller holding the first one's typed amount.
  // An earlier version of this note asserted an equal-life case on the pay arm instead; that path
  // was never demonstrated, and this one is read straight off the field initializers.
  const sourceId = data?.source_id ?? null;
  const promptPlayer = data?.player ?? null;
  const [raw, setRaw] = useState(String(min));

  useEffect(() => {
    if (isPayAmount) setRaw(String(min));
  }, [isPayAmount, min, max, sourceId, promptPlayer]);

  const amount = parseAmount(raw, min, max);

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
      case "LoopCollapse":
        // Dead arm: LoopCollapse never reaches resourceLabel (the loopCollapseAxis
        // branch owns its title/button). Kept for switch exhaustiveness only.
        return "";
    }
  }, [data, t]);

  // Display-only: the LoopCollapse prompt frames axis growth (tokens / counters /
  // life / mixed), not a cost to "pay" (CR 732.2a loop collapse — the engine
  // applies N cycles of the collapsed axis, nothing is spent). Counter/life labels
  // are ITERATION-framed (×N), never a raw resource count: N is the cycle count and
  // each cycle applies per_cycle_delta, so N tokens but N×δ counters/life.
  const loopCollapseAxis =
    data?.resource.type === "LoopCollapse" ? data.resource.data.axis : null;

  const handleCommit = useCallback(() => {
    // Sanitization gate: an unparsed/out-of-range entry never reaches the engine.
    // `amount === null`, NOT `!amount` — 0 is a legal amount on every PayAmountChoice minting
    // site (all six hardcode `min: 0`).
    if (amount === null) return;
    dispatch({ type: "SubmitPayAmount", data: { amount } });
  }, [dispatch, amount]);

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
            {t(
              loopCollapseAxis
                ? `mana.loopCollapseTitle${loopCollapseAxis}`
                : "mana.chooseAmountTitle",
            )}
            {sourceName && (
              <span className="ml-1 text-gray-400">&mdash; {sourceName}</span>
            )}
          </h3>

          <AmountInput
            raw={raw}
            onRawChange={setRaw}
            min={min}
            max={max}
            onSubmit={handleCommit}
            labels={{
              input: t("mana.chooseAmountAria"),
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
              {/* No valid amount ⇒ name the action without a value. `amount ?? min` here would
                  label the button "Create 0 tokens" while the player has 1001 typed. */}
              {amount === null
                ? t("mana.confirmAmount")
                : loopCollapseAxis
                  ? // `count` drives i18next pluralization for the Tokens axis
                    // (loopCollapseAmountTokens_one/_other); the ×N-framed
                    // Counters/Life/Mixed keys keep reading `value`.
                    t(`mana.loopCollapseAmount${loopCollapseAxis}`, {
                      value: amount,
                      count: amount,
                    })
                  : t("mana.payAmount", {
                      value: amount,
                      resource: resourceLabel,
                    })}
            </button>
          </div>
        </div>
      </motion.div>
    </AnimatePresence>
  );
}
