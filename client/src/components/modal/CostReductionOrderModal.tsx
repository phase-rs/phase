import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  CostReductionEntry,
  CostReductionOutcome,
  ManaCost,
  ManaCostShard,
} from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ManaCostSymbols } from "../mana/ManaCostSymbols.tsx";
import { DialogShell } from "./DialogShell.tsx";

const EMPTY_REDUCTIONS: CostReductionEntry[] = [];
const EMPTY_OUTCOMES: CostReductionOutcome[] = [];
const EMPTY_SHARDS: ManaCostShard[] = [];

/**
 * CR 601.2b + CR 601.2f: "If multiple cost reductions apply, the player may
 * apply them in any order", and "if a cost ... includes hybrid mana symbols,
 * the player announces the nonhybrid equivalent cost they intend to pay."
 * Surfaced when the local player is casting a spell whose cost-determination
 * choices are *observable* — when two legal elections lock in genuinely
 * different total costs.
 *
 * The engine owns ALL of the arithmetic. This component permutes an index array
 * (the `reductions` payload the engine provided), carries the engine-authored
 * hybrid announcement alongside it, and dispatches both via
 * `GameAction::OrderCostReductions`. It never computes a cost: the total shown
 * for the current arrangement is looked up in the engine's `outcomes` list, and
 * when the arrangement is not itself one of the engine's representatives the
 * total is simply not claimed. No re-derivation from `state.objects`, no mana
 * arithmetic.
 */
export function CostReductionOrderModal() {
  const { t } = useTranslation("game");
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  const isOrdering = waitingFor?.type === "OrderCostReductions";
  const reductions = isOrdering ? waitingFor.data.reductions : EMPTY_REDUCTIONS;
  const outcomes = isOrdering ? waitingFor.data.outcomes : EMPTY_OUTCOMES;
  const hybridSymbols = isOrdering
    ? (waitingFor.data.hybrid_symbols ?? EMPTY_SHARDS)
    : EMPTY_SHARDS;

  // Local UI state: the chosen permutation (indices into `reductions`) and the
  // announcement it goes with. Both seeded from the engine's caster-optimal
  // representative (`outcomes` is sorted cheapest-first) so the default is the
  // one the engine used to pick silently, and re-seeded whenever a new prompt
  // arrives.
  const [order, setOrder] = useState<number[]>(() =>
    seedOrder(reductions, outcomes),
  );
  const [announcement, setAnnouncement] = useState<ManaCostShard[]>(
    () => outcomes[0]?.hybrid_announcement ?? EMPTY_SHARDS,
  );
  useEffect(() => {
    setOrder(seedOrder(reductions, outcomes));
    setAnnouncement(outcomes[0]?.hybrid_announcement ?? EMPTY_SHARDS);
  }, [reductions, outcomes, isOrdering]);

  const move = useCallback((from: number, to: number) => {
    setOrder((prev) => {
      if (to < 0 || to >= prev.length) return prev;
      const next = prev.slice();
      const [item] = next.splice(from, 1);
      next.splice(to, 0, item);
      return next;
    });
  }, []);

  const applyOutcome = useCallback((outcome: CostReductionOutcome) => {
    setOrder(outcome.order.slice());
    setAnnouncement((outcome.hybrid_announcement ?? EMPTY_SHARDS).slice());
  }, []);

  const handleConfirm = useCallback(() => {
    dispatch({
      type: "OrderCostReductions",
      data: { order, hybrid_announcement: announcement },
    });
  }, [dispatch, order, announcement]);

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
  }, [dispatch]);

  if (!isOrdering || reductions.length === 0) return null;

  const lockedCost = lockedCostForElection(order, announcement, outcomes);

  return (
    <DialogShell
      eyebrow={t("costReductionOrder.eyebrow")}
      title={t("costReductionOrder.title")}
      subtitle={t("costReductionOrder.subtitle")}
      size="md"
      scrollable
      footer={
        <div className="flex w-full items-center justify-between gap-3">
          <button
            type="button"
            onClick={handleCancel}
            className="min-h-11 rounded-[16px] border border-white/15 px-5 py-3 font-semibold text-white/70 transition hover:bg-white/10"
          >
            {t("costReductionOrder.cancelCast")}
          </button>
          <button
            type="button"
            onClick={handleConfirm}
            className="min-h-11 rounded-[16px] bg-cyan-500/80 px-5 py-3 font-semibold text-white transition hover:bg-cyan-500"
          >
            {t("costReductionOrder.confirmOrder")}
          </button>
        </div>
      }
    >
      <div className="px-3 py-3 lg:px-5 lg:py-5">
        <div className="mb-2 text-xs uppercase tracking-wide text-white/50">
          {t("costReductionOrder.appliedFirst")}
        </div>
        <ol className="flex flex-col gap-2">
          {order.map((engineIndex, position) => {
            const reduction = reductions[engineIndex];
            return (
              <li
                key={`${engineIndex}-${reduction.display_name}`}
                className="flex items-start gap-2 rounded-[16px] border border-white/8 bg-white/5 px-4 py-3"
              >
                <div className="flex-1 text-left">
                  <div className="font-semibold text-white">
                    {reduction.display_name ||
                      t("costReductionOrder.reductionFallback", {
                        number: engineIndex + 1,
                      })}
                  </div>
                  <div className="flex items-center gap-1 text-sm text-white/70">
                    <ManaCostSymbols cost={reduction.amount} size="xs" />
                    {reduction.multiplier > 1 && (
                      <span>
                        {t("costReductionOrder.multiplier", {
                          count: reduction.multiplier,
                        })}
                      </span>
                    )}
                  </div>
                  <div className="text-xs text-white/50">
                    {reduction.reach === "ColoredManaOnly"
                      ? t("costReductionOrder.reachColoredOnly")
                      : t("costReductionOrder.reachSpills")}
                  </div>
                </div>
                <div className="flex flex-col gap-1">
                  <button
                    type="button"
                    aria-label={t("costReductionOrder.moveUp")}
                    disabled={position === 0}
                    onClick={() => move(position, position - 1)}
                    className="min-h-8 rounded border border-white/10 px-2 text-white/80 transition hover:bg-white/10 disabled:opacity-30"
                  >
                    ▲
                  </button>
                  <button
                    type="button"
                    aria-label={t("costReductionOrder.moveDown")}
                    disabled={position === order.length - 1}
                    onClick={() => move(position, position + 1)}
                    className="min-h-8 rounded border border-white/10 px-2 text-white/80 transition hover:bg-white/10 disabled:opacity-30"
                  >
                    ▼
                  </button>
                </div>
              </li>
            );
          })}
        </ol>
        <div className="mt-2 text-xs uppercase tracking-wide text-white/50">
          {t("costReductionOrder.appliedLast")}
        </div>

        {hybridSymbols.length > 0 && (
          <div className="mt-4 rounded-[16px] border border-white/8 bg-white/5 px-4 py-3">
            <div className="text-xs uppercase tracking-wide text-white/50">
              {t("costReductionOrder.announcedHybrids")}
            </div>
            <div className="mt-1 flex flex-wrap items-center gap-2 text-white">
              {hybridSymbols.map((symbol, position) => (
                <span
                  key={`${symbol}-${position}`}
                  className="flex items-center gap-1 text-sm text-white/80"
                >
                  <ManaCostSymbols
                    cost={{ type: "Cost", shards: [symbol], generic: 0 }}
                    size="xs"
                  />
                  <span aria-hidden>→</span>
                  {announcement[position] ? (
                    <ManaCostSymbols
                      cost={{
                        type: "Cost",
                        shards: [announcement[position]],
                        generic: 0,
                      }}
                      size="xs"
                    />
                  ) : (
                    <ManaCostSymbols
                      cost={{ type: "Cost", shards: [symbol], generic: 0 }}
                      size="xs"
                    />
                  )}
                </span>
              ))}
            </div>
          </div>
        )}

        <div className="mt-4 rounded-[16px] border border-white/8 bg-white/5 px-4 py-3">
          <div className="text-xs uppercase tracking-wide text-white/50">
            {t("costReductionOrder.lockedCost")}
          </div>
          <div className="mt-1 text-white">
            {lockedCost === null ? (
              t("costReductionOrder.lockedCostUnknown")
            ) : (
              <ManaCostSymbols cost={lockedCost} size="sm" />
            )}
          </div>
        </div>

        <div className="mt-4">
          <div className="mb-2 text-xs uppercase tracking-wide text-white/50">
            {t("costReductionOrder.outcomesHeading")}
          </div>
          <div className="flex flex-wrap gap-2">
            {outcomes.map((outcome) => (
              <button
                key={`${outcome.order.join("-")}|${(outcome.hybrid_announcement ?? []).join("-")}`}
                type="button"
                onClick={() => applyOutcome(outcome)}
                className="min-h-9 rounded-[12px] border border-white/10 px-3 py-2 text-sm text-white/80 transition hover:bg-white/10"
              >
                <ManaCostSymbols cost={outcome.locked_cost} size="xs" />
              </button>
            ))}
          </div>
        </div>
      </div>
    </DialogShell>
  );
}

/**
 * The engine's caster-optimal representative, or the identity permutation when
 * the prompt somehow carries no outcome. Never computes an order of its own.
 */
function seedOrder(
  reductions: CostReductionEntry[],
  outcomes: CostReductionOutcome[],
): number[] {
  const first = outcomes[0];
  if (first) return first.order.slice();
  return reductions.map((_, index) => index);
}

/**
 * The total cost the engine attributed to this exact election, or `null` when
 * the arrangement is not one of the engine's representatives. Returning `null`
 * rather than guessing is the point: the frontend must never assert a cost the
 * engine did not author.
 */
function lockedCostForElection(
  order: number[],
  announcement: ManaCostShard[],
  outcomes: CostReductionOutcome[],
): ManaCost | null {
  const match = outcomes.find(
    (outcome) =>
      outcome.order.length === order.length &&
      outcome.order.every((index, position) => index === order[position]) &&
      sameAnnouncement(outcome.hybrid_announcement ?? [], announcement),
  );
  return match ? match.locked_cost : null;
}

function sameAnnouncement(
  a: ManaCostShard[],
  b: ManaCostShard[],
): boolean {
  return a.length === b.length && a.every((shard, index) => shard === b[index]);
}
