import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  ManaType,
  PhyrexianShard,
  ShardChoice,
} from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import {
  abilityCostToManaShards,
  manaCostToShards,
  SHARD_ABBREVIATION,
} from "../../viewmodel/costLabel.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { useIsNarrowViewport } from "../modal/DialogHost.tsx";
import { PeekTab } from "../modal/DialogShell.tsx";
import { useOptionalDialogPeek } from "../modal/dialogPeekContext.ts";
import { ManaBadge } from "./ManaBadge.tsx";
import { ManaSymbol } from "./ManaSymbol.tsx";

const MANA_ORDER: ManaType[] = ["White", "Blue", "Black", "Red", "Green", "Colorless"];

// Hybrid/Phyrexian shards still resolve interactively during ManaPayment.
// X no longer appears here — `ChooseXValueUI` handles X selection before
// payment (CR 601.2f) and concretizes the cost, so any `ManaCostShard::X`
// has already been replaced with generic by the time this UI renders.
function hasAmbiguousCost(shards: string[]): boolean {
  return shards.some((s) => s.includes("/"));
}

export function ManaPaymentUI() {
  const { t } = useTranslation("game");
  const waitingFor = useGameStore((s) => s.waitingFor);
  const gameState = useGameStore((s) => s.gameState);
  const dispatch = useGameStore((s) => s.dispatch);
  const canAct = useCanActForWaitingState();

  const isManaPayment = waitingFor?.type === "ManaPayment";
  const isPhyrexianPayment = waitingFor?.type === "PhyrexianPayment";
  const isAnyPayment = isManaPayment || isPhyrexianPayment;
  const playerId = isManaPayment
    ? waitingFor.data.player
    : isPhyrexianPayment
      ? waitingFor.data.player
      : null;
  const convokeMode = isManaPayment ? waitingFor.data.convoke_mode : undefined;
  const player = playerId != null ? gameState?.players[playerId] : null;

  // CR 702.51a: the bottom-anchored convoke panel can sit on top of the very
  // creatures the player must tap to pay. The DialogHost peek affordance lets
  // them slide it out of the way and back (a no-op for plain mana payment, which
  // needs no board interaction). `useOptionalDialogPeek` is null outside a host.
  const peek = useOptionalDialogPeek();
  const isNarrow = useIsNarrowViewport();

  // CR 107.4f + CR 601.2f: Engine-provided per-shard options for Phyrexian payment.
  // The UI maps shard_index → PhyrexianShard so it can disable toggles for trivial
  // shards (ManaOnly / LifeOnly) and only accept toggles on ManaOrLife shards.
  const phyrexianShards: PhyrexianShard[] = useMemo(
    () => (isPhyrexianPayment ? waitingFor.data.shards : []),
    [isPhyrexianPayment, waitingFor],
  );
  const spellObjectId = isPhyrexianPayment ? waitingFor.data.spell_object : null;

  // CR 601.2f/601.2g: The cost being paid is the engine-resolved locked-in
  // total carried in `GameState::pending_cast.cost` (base mana cost + Strive
  // surcharge + RaiseCost statics + commander tax - reductions). Falling back
  // to the printed `mana_cost` of the stack/spell object only when no pending
  // cast is present keeps the panel correct for cost-modified spells without
  // any frontend cost computation.
  const costShards = useMemo(() => {
    if (!gameState) return null;
    if (gameState.pending_cast) {
      // Activated-ability mana payment: prefer `activation_cost` when present.
      // The engine reuses PendingCast for both spell casts and activated abilities;
      // for the latter, the mana to be paid is stored on `activation_cost`.
      if (gameState.pending_cast.activation_cost) {
        return abilityCostToManaShards(gameState.pending_cast.activation_cost) ?? [];
      }
      return manaCostToShards(gameState.pending_cast.cost);
    }
    if (isPhyrexianPayment && spellObjectId != null) {
      const sourceObj = gameState.objects[spellObjectId];
      if (!sourceObj || sourceObj.mana_cost.type !== "Cost") return null;
      return manaCostToShards(sourceObj.mana_cost);
    }
    if (isManaPayment) {
      const stack = gameState.stack;
      if (stack.length === 0) return null;
      const topEntry = stack[stack.length - 1];
      const sourceObj = gameState.objects[topEntry.source_id];
      if (!sourceObj || sourceObj.mana_cost.type !== "Cost") return null;
      return manaCostToShards(sourceObj.mana_cost);
    }
    return null;
  }, [gameState, isManaPayment, isPhyrexianPayment, spellObjectId]);

  const cardName = useMemo(() => {
    if (!gameState) return null;
    if (isPhyrexianPayment && spellObjectId != null) {
      return gameState.objects[spellObjectId]?.name ?? null;
    }
    if (isManaPayment && gameState.pending_cast) {
      return gameState.objects[gameState.pending_cast.object_id]?.name ?? null;
    }
    if (isManaPayment) {
      const stack = gameState.stack;
      if (stack.length === 0) return null;
      const topEntry = stack[stack.length - 1];
      return gameState.objects[topEntry.source_id]?.name ?? null;
    }
    return null;
  }, [gameState, isManaPayment, isPhyrexianPayment, spellObjectId]);

  const isAmbiguous = costShards != null && hasAmbiguousCost(costShards);

  // Local state for ambiguous cost choices (hybrid/phyrexian).
  const [phyrexianChoices, setPhyrexianChoices] = useState<Map<number, "mana" | "life">>(
    () => new Map(),
  );
  const [hybridChoices, setHybridChoices] = useState<Map<number, string>>(
    () => new Map(),
  );

  // CR 107.4f + CR 601.2f: Initialize Phyrexian toggles from engine-provided
  // `ShardOptions`. Trivial shards (ManaOnly/LifeOnly) are pre-filled and locked.
  useEffect(() => {
    if (isPhyrexianPayment) {
      const next = new Map<number, "mana" | "life">();
      for (const shard of phyrexianShards) {
        if (shard.options.type === "LifeOnly") {
          next.set(shard.shard_index, "life");
        } else {
          next.set(shard.shard_index, "mana");
        }
      }
      setPhyrexianChoices(next);
      setHybridChoices(new Map());
    } else {
      setPhyrexianChoices(new Map());
      setHybridChoices(new Map());
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [costShards, isPhyrexianPayment]);

  // Summarize mana pool by color
  const manaPoolSummary = useMemo(() => {
    if (!player) return [];
    const counts: Record<ManaType, number> = {
      White: 0, Blue: 0, Black: 0, Red: 0, Green: 0, Colorless: 0,
    };
    for (const unit of player.mana_pool.mana) {
      if (unit.restrictions.includes("ConvokePayment")) continue;
      counts[unit.color]++;
    }
    return MANA_ORDER.filter((c) => counts[c] > 0).map((c) => ({ color: c, amount: counts[c] }));
  }, [player]);

  // CR 702.51a / CR 702.126a: each creature/artifact tapped for convoke/improvise
  // adds a `ConvokePayment`-restricted marker to the pool (engine
  // `ManaUnit::convoke_payment`). These are deliberately excluded from
  // `manaPoolSummary` above because they are not spendable mana — only a record
  // that the permanent has been tapped toward this cost. Surface them in their
  // own row so each tap gives the player visible feedback (otherwise the panel
  // looks frozen as creatures are tapped).
  const convokeStaged = useMemo(() => {
    if (!player) return [];
    const counts: Record<ManaType, number> = {
      White: 0, Blue: 0, Black: 0, Red: 0, Green: 0, Colorless: 0,
    };
    for (const unit of player.mana_pool.mana) {
      if (!unit.restrictions.includes("ConvokePayment")) continue;
      counts[unit.color]++;
    }
    return MANA_ORDER.filter((c) => counts[c] > 0).map((c) => ({ color: c, amount: counts[c] }));
  }, [player]);

  // CR 107.4f + CR 601.2f: Only shards with `ManaOrLife` can be toggled; ManaOnly
  // / LifeOnly shards are locked to their single legal payment.
  const shardByIndex = useMemo(() => {
    const map = new Map<number, PhyrexianShard>();
    for (const shard of phyrexianShards) {
      map.set(shard.shard_index, shard);
    }
    return map;
  }, [phyrexianShards]);

  const togglePhyrexian = useCallback(
    (idx: number) => {
      if (isPhyrexianPayment) {
        const shard = shardByIndex.get(idx);
        if (shard && shard.options.type !== "ManaOrLife") return;
      }
      setPhyrexianChoices((prev) => {
        const next = new Map(prev);
        next.set(idx, next.get(idx) === "life" ? "mana" : "life");
        return next;
      });
    },
    [isPhyrexianPayment, shardByIndex],
  );

  const toggleHybrid = useCallback(
    (idx: number, shard: string) => {
      const [a, b] = shard.split("/");
      setHybridChoices((prev) => {
        const next = new Map(prev);
        next.set(idx, next.get(idx) === b ? a : b);
        return next;
      });
    },
    [],
  );

  const handlePay = useCallback(() => {
    if (isPhyrexianPayment) {
      // CR 107.4f + CR 601.2f: Submit the per-shard choices in shard order.
      const choices: ShardChoice[] = phyrexianShards.map((shard) => {
        const picked = phyrexianChoices.get(shard.shard_index) ?? "mana";
        return picked === "life" ? { type: "PayLife" } : { type: "PayMana" };
      });
      dispatch({ type: "SubmitPhyrexianChoices", data: { choices } });
      return;
    }
    dispatch({ type: "PassPriority" });
  }, [dispatch, isPhyrexianPayment, phyrexianChoices, phyrexianShards]);

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
  }, [dispatch]);

  // Total life cost from phyrexian choices
  const lifeCost = useMemo(() => {
    let cost = 0;
    for (const choice of phyrexianChoices.values()) {
      if (choice === "life") cost += 2;
    }
    return cost;
  }, [phyrexianChoices]);

  // Don't render if not a mana/Phyrexian payment the local player can act on.
  // CR 601.2g + CR 107.4f: mana payment and Phyrexian per-shard choice are
  // decisions for the caster alone; opponents see the mid-cast state via the
  // stack display, not an interactive panel.
  if (!isAnyPayment || !player || !canAct) return null;

  return (
    <AnimatePresence>
      <motion.div
        className="pointer-events-none fixed inset-x-0 bottom-0 z-40 flex justify-center pb-4"
        initial={{ y: 80, opacity: 0 }}
        animate={{ y: 0, opacity: 1 }}
        exit={{ y: 80, opacity: 0 }}
        transition={{ duration: 0.25 }}
      >
        {/* `pointer-events-auto` re-enables clicks on the panel itself: under a
            click-through convoke/improvise host (CR 702.51a / 702.126a) the
            DialogHost wrapper is `pointer-events: none` so board taps reach the
            artifacts/creatures, but the Pay/Cancel controls must still respond.
            The surrounding full-width strip stays pass-through. */}
        <div className="pointer-events-auto relative rounded-xl bg-gray-900/95 p-4 shadow-2xl ring-1 ring-gray-700 min-w-[280px] max-w-[420px]">
          {/* CR 702.51a: collapse cue for convoke/improvise payment — slides this
              panel off any creature it overlaps so the player can tap it. Only
              shown while the panel is in place (peeked state surfaces the
              DialogHost restore tab instead). */}
          {convokeMode && peek && !peek.peeked && (
            <PeekTab
              direction={isNarrow ? "bottom" : "right"}
              onClick={peek.togglePeek}
            />
          )}
          <h3 className="mb-3 text-center text-sm font-semibold text-gray-300">
            {t("mana.payMana")}
            {cardName && (
              <span className="ml-1 text-gray-400">
                &mdash; {cardName}
              </span>
            )}
          </h3>

          {costShards && (
            <>
              {/* Cost display row */}
              <div className="mb-3 flex items-center justify-center gap-1.5">
                {costShards.map((shard, idx) => (
                  <ManaSymbol key={idx} shard={shard} size="lg" />
                ))}
              </div>

              {convokeMode && (
                <p className="mb-3 text-center text-xs font-medium text-cyan-300">
                  {convokeMode === "Convoke"
                    ? t("mana.convokeHint")
                    : convokeMode === "Improvise"
                      ? t("mana.improviseHint")
                      : convokeMode === "Delve"
                        ? t("mana.delveHint")
                        : t("mana.convokeOrImproviseHint")}
                </p>
              )}

              {/* CR 702.51a: live feedback for staged convoke/improvise taps —
                  each tapped permanent shows here as a cyan-tinted badge so the
                  player can see the payment progressing as the cost is covered. */}
              {convokeMode && convokeStaged.length > 0 && (
                <div className="mb-3 flex items-center justify-center gap-2">
                  <span className="text-xs text-cyan-400">
                    {t("mana.convokeStaged")}
                  </span>
                  {convokeStaged.map(({ color, amount }) => (
                    <ManaBadge key={color} color={color} amount={amount} />
                  ))}
                </div>
              )}

              {/* Phyrexian toggles — during PhyrexianPayment we iterate the
                  engine-provided `shards` list (keyed by `shard_index` into
                  cost.shards); during legacy ManaPayment we scan costShards
                  for "/P" and index by the display array. */}
              {isPhyrexianPayment && phyrexianShards.length > 0 && (
                <div className="mb-3 flex flex-wrap items-center justify-center gap-2">
                  {phyrexianShards.map((shard) => {
                    const payLife =
                      phyrexianChoices.get(shard.shard_index) === "life";
                    const locked = shard.options.type !== "ManaOrLife";
                    const manaAbbrev =
                      SHARD_ABBREVIATION[`Phyrexian${shard.color}`] ??
                      `${shard.color[0]}/P`;
                    return (
                      <button
                        key={shard.shard_index}
                        onClick={() => togglePhyrexian(shard.shard_index)}
                        disabled={locked}
                        className={`flex items-center gap-1 rounded-md px-2 py-1 text-xs ring-1 transition ${
                          locked ? "cursor-not-allowed opacity-60" : ""
                        } ${
                          payLife
                            ? "bg-red-900/60 text-red-300 ring-red-500/40"
                            : "bg-gray-800 text-gray-300 ring-gray-600"
                        }`}
                      >
                        {payLife ? (
                          <>
                            <span aria-label="heart">&#x2764;</span>
                            <span>{t("mana.lifeAmount")}</span>
                          </>
                        ) : (
                          <ManaSymbol shard={manaAbbrev} size="sm" />
                        )}
                      </button>
                    );
                  })}
                  {lifeCost > 0 && (
                    <span className="text-xs text-red-400">
                      {t("mana.lifeCostSummary", { count: lifeCost })}
                    </span>
                  )}
                </div>
              )}
              {!isPhyrexianPayment &&
                isAmbiguous &&
                costShards.some((s) => s.endsWith("/P")) && (
                  <div className="mb-3 flex flex-wrap items-center justify-center gap-2">
                    {costShards.map((shard, idx) => {
                      if (!shard.endsWith("/P")) return null;
                      const payLife = phyrexianChoices.get(idx) === "life";
                      return (
                        <button
                          key={idx}
                          onClick={() => togglePhyrexian(idx)}
                          className={`flex items-center gap-1 rounded-md px-2 py-1 text-xs ring-1 transition ${
                            payLife
                              ? "bg-red-900/60 text-red-300 ring-red-500/40"
                              : "bg-gray-800 text-gray-300 ring-gray-600"
                          }`}
                        >
                          {payLife ? (
                            <>
                              <span aria-label="heart">&#x2764;</span>
                              <span>2 life</span>
                            </>
                          ) : (
                            <ManaSymbol shard={shard} size="sm" />
                          )}
                        </button>
                      );
                    })}
                    {lifeCost > 0 && (
                      <span className="text-xs text-red-400">
                        ({lifeCost} life)
                      </span>
                    )}
                  </div>
                )}

              {/* Hybrid toggles */}
              {isAmbiguous && costShards.some(
                (s) => s.includes("/") && !s.endsWith("/P"),
              ) && (
                <div className="mb-3 flex flex-wrap items-center justify-center gap-2">
                  {costShards.map((shard, idx) => {
                    if (!shard.includes("/") || shard.endsWith("/P")) return null;
                    const [a, b] = shard.split("/");
                    const chosen = hybridChoices.get(idx) ?? a;
                    return (
                      <button
                        key={idx}
                        onClick={() => toggleHybrid(idx, shard)}
                        className="flex items-center gap-1 rounded-md bg-gray-800 px-2 py-1 ring-1 ring-gray-600 transition hover:ring-gray-400"
                      >
                        <ManaSymbol
                          shard={chosen}
                          size="sm"
                          className={chosen === a ? "opacity-100" : "opacity-40"}
                        />
                        <span className="text-[10px] text-gray-500">/</span>
                        <ManaSymbol
                          shard={chosen === a ? b : a}
                          size="sm"
                          className="opacity-40"
                        />
                      </button>
                    );
                  })}
                </div>
              )}
            </>
          )}

          {!costShards && (
            <p className="mb-3 text-center text-xs text-gray-400">
              {t("mana.paymentPending")}
            </p>
          )}

          {/* Current mana pool */}
          <div className="mb-3 flex items-center justify-center gap-2">
            <span className="text-xs text-gray-500">{t("mana.poolLabel")}</span>
            {manaPoolSummary.length > 0 ? (
              manaPoolSummary.map(({ color, amount }) => (
                <ManaBadge key={color} color={color} amount={amount} />
              ))
            ) : (
              <span className="text-xs text-gray-600">{t("mana.poolEmpty")}</span>
            )}
          </div>

          {/* Confirm / Cancel buttons */}
          <div className="flex justify-center gap-3">
            <button
              onClick={handlePay}
              className={gameButtonClass({ tone: "emerald", size: "md" })}
            >
              {t("mana.pay")}
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
