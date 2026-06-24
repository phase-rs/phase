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
import {
  canonGrant,
  canonRestriction,
  manaGroupTooltip,
} from "../../viewmodel/manaPoolGroups.ts";
import type { ManaRestriction, ManaSpellGrant } from "../../adapter/types.ts";

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

  // CR 118.3a: spendable pool units grouped by (color, restrictions, grants,
  // source). Each group renders as one `N×{symbol}` chip labelled with its
  // source permanent, so a large pool stays compact and rules-relevant mana
  // (Delighted Halfling / Cavern of Souls — distinct grants AND a named source)
  // is its own directable chip. Convoke markers are excluded (not spendable).
  // The frontend resolves the source NAME for display only; it computes no
  // eligibility — the engine accepts or rejects each pin.
  const paymentGroups = useMemo<PaymentGroup[]>(() => {
    if (!player || !gameState) return [];
    const groups = new Map<string, PaymentGroup>();
    for (const unit of player.mana_pool.mana) {
      if (unit.restrictions.includes("ConvokePayment")) continue;
      const grants = unit.grants ?? [];
      const sourceName = gameState.objects[unit.source_id]?.name ?? null;
      const key = JSON.stringify([
        unit.color,
        [...unit.restrictions].map(canonRestriction).sort(),
        [...grants].map(canonGrant).sort(),
        sourceName,
      ]);
      const existing = groups.get(key);
      if (existing) {
        existing.pipIds.push(unit.pip_id);
      } else {
        groups.set(key, {
          key,
          color: unit.color,
          restrictions: unit.restrictions,
          grants,
          special: unit.restrictions.length > 0 || grants.length > 0,
          sourceName,
          pipIds: [unit.pip_id],
        });
      }
    }
    return [...groups.values()].sort((a, b) => {
      const colorDelta = MANA_ORDER.indexOf(a.color) - MANA_ORDER.indexOf(b.color);
      if (colorDelta !== 0) return colorDelta;
      return (a.special ? 1 : 0) - (b.special ? 1 : 0);
    });
  }, [player, gameState]);

  // CR 118.3a + CR 601.2g: engine-computed cost still unpaid by the units the
  // player has selected so far (`null` when not exposed — e.g. ambiguous/
  // activation/Phyrexian). The frontend never computes this — it renders the
  // engine's residual so the cost visibly shrinks as mana is picked.
  const remainingShards = useMemo(() => {
    const remaining = gameState?.derived?.pending_payment_remaining;
    return remaining ? manaCostToShards(remaining) : null;
  }, [gameState]);

  // CR 118.3a: the engine records player-directed pins on `pending_cast`.
  // The frontend is a pure mirror — it renders the engine's pin set and the
  // individual pool units, and dispatches Spend/UnspendPoolMana on click. It
  // computes no eligibility (the engine accepts or rejects each pin).
  const pinnedPipIds = useMemo(
    () => new Set(gameState?.pending_cast?.pinned_pool_units ?? []),
    [gameState],
  );

  const pinUnit = useCallback(
    (pipId: number) => dispatch({ type: "SpendPoolMana", data: { pip_id: pipId } }),
    [dispatch],
  );
  const unpinUnit = useCallback(
    (pipId: number) => dispatch({ type: "UnspendPoolMana", data: { pip_id: pipId } }),
    [dispatch],
  );
  // Shift / fill: pin several units of one group at once. Over-pinning is safe —
  // the engine spends pinned units first only up to the cost (CR 118.3a), so any
  // extra pinned mana simply stays in the pool at finalize.
  const pinPips = useCallback(
    (pipIds: number[]) => {
      for (const pipId of pipIds) {
        dispatch({ type: "SpendPoolMana", data: { pip_id: pipId } });
      }
    },
    [dispatch],
  );

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
              {/* Cost display row — for a plain (non-ambiguous) cost we show the
                  cost STILL TO PAY after the player's current selection
                  (engine-computed `pending_payment_remaining`), so it visibly
                  shrinks as mana is picked and reads "covered" once the
                  selection pays the whole cost. Ambiguous (hybrid/Phyrexian)
                  costs keep the full-cost display because their per-shard
                  toggles index the full shard list. */}
              <div className="mb-3 flex items-center justify-center gap-1.5">
                {!isAmbiguous && remainingShards != null ? (
                  remainingShards.length > 0 ? (
                    remainingShards.map((shard, idx) => (
                      <ManaSymbol key={idx} shard={shard} size="lg" />
                    ))
                  ) : (
                    <span className="text-sm font-semibold text-emerald-400">
                      {t("mana.covered")}
                    </span>
                  )
                ) : (
                  costShards.map((shard, idx) => (
                    <ManaSymbol key={idx} shard={shard} size="lg" />
                  ))
                )}
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

          {/* CR 118.3a: two-pile mana selection. Each fungible group renders as
              one `N×{symbol}` chip (count + source permanent) so a large pool
              stays compact. Tapping an AVAILABLE chip moves one unit to SPENDING
              (shift-click moves all of that group); the engine spends pinned
              units first, so a selection covering the cost pays with exactly
              those units — and applies any rider they carry (Cavern of Souls /
              Delighted Halfling "can't be countered"). Tapping a SPENDING chip
              returns one. Phyrexian payment shows the pool read-only (no piles). */}
          <div className="mb-3 space-y-2">
            {paymentGroups.length === 0 && (
              <div className="flex items-center justify-center gap-2">
                <span className="text-xs text-gray-500">{t("mana.poolLabel")}</span>
                <span className="text-xs text-gray-600">{t("mana.poolEmpty")}</span>
              </div>
            )}

            {paymentGroups.length > 0 && !isManaPayment && (
              <div className="flex flex-wrap items-center justify-center gap-1.5">
                {paymentGroups.map((group) => (
                  <ManaChip key={group.key} group={group} count={group.pipIds.length} />
                ))}
              </div>
            )}

            {paymentGroups.length > 0 && isManaPayment && (
              <>
                <div>
                  <p className="mb-1 text-center text-[11px] text-gray-500">
                    {t("mana.spendHint")}
                    <span className="ml-1 text-gray-600">{t("mana.fillHint")}</span>
                  </p>
                  <div className="flex flex-wrap items-center justify-center gap-1.5">
                    {paymentGroups.map((group) => {
                      const ids = group.pipIds.filter((id) => !pinnedPipIds.has(id));
                      if (ids.length === 0) return null;
                      return (
                        <ManaChip
                          key={group.key}
                          group={group}
                          count={ids.length}
                          onActivate={(fill) => (fill ? pinPips(ids) : pinUnit(ids[0]))}
                        />
                      );
                    })}
                  </div>
                </div>

                {pinnedPipIds.size > 0 && (
                  <div className="rounded-lg bg-cyan-500/5 px-2 py-1.5 ring-1 ring-cyan-400/20">
                    <p className="mb-1 text-center text-[11px] text-cyan-300/80">
                      {t("mana.spending")}
                    </p>
                    <div className="flex flex-wrap items-center justify-center gap-1.5">
                      {paymentGroups.map((group) => {
                        const ids = group.pipIds.filter((id) => pinnedPipIds.has(id));
                        if (ids.length === 0) return null;
                        return (
                          <ManaChip
                            key={group.key}
                            group={group}
                            count={ids.length}
                            selected
                            onActivate={() => unpinUnit(ids[ids.length - 1])}
                          />
                        );
                      })}
                    </div>
                  </div>
                )}
              </>
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
              className={gameButtonClass({ tone: "slate", size: "md" })}
            >
              {t("common:actions.cancel")}
            </button>
          </div>
        </div>
      </motion.div>
    </AnimatePresence>
  );
}

// Color → shard symbol code for `ManaSymbol` (White→"W", …, Colorless→"C").
const COLOR_SHARD: Record<ManaType, string> = {
  White: "W",
  Blue: "U",
  Black: "B",
  Red: "R",
  Green: "G",
  Colorless: "C",
};

/**
 * A run of fungible pool units the payment panel renders as one chip — same
 * color, spend restrictions, spell grants, AND source permanent. `pipIds` holds
 * the concrete unit ids (the pin targets); `sourceName` is resolved for display
 * only. Grouping by source keeps a large pool compact while still giving
 * rules-relevant mana (Delighted Halfling, Cavern of Souls) its own labeled chip.
 */
interface PaymentGroup {
  key: string;
  color: ManaType;
  restrictions: ManaRestriction[];
  grants: ManaSpellGrant[];
  special: boolean;
  sourceName: string | null;
  pipIds: number[];
}

interface ManaChipProps {
  group: PaymentGroup;
  /** How many units of the group this chip represents (available or spending). */
  count: number;
  selected?: boolean;
  /** Present → interactive. `fill` is true on shift-click (act on all, not one). */
  onActivate?: (fill: boolean) => void;
}

/**
 * CR 118.3a: one fungible group as `N×{symbol}` + source label. Clicking pins or
 * returns a unit of the group; shift-click acts on all of it. The amber dot +
 * tooltip surface any spend restriction / spell grant (e.g. "legendary-only;
 * uncounterable") so the player can direct rider-bearing mana on purpose.
 */
function ManaChip({ group, count, selected = false, onActivate }: ManaChipProps) {
  const { t } = useTranslation("game");
  // Combine the restriction/grant detail with the source name so neither is
  // hidden when both exist (the inline source label may be truncated).
  const tooltip =
    [manaGroupTooltip((k) => t(k), group), group.sourceName]
      .filter(Boolean)
      .join(" — ") || undefined;

  const ring = selected
    ? "bg-cyan-400/15 ring-1 ring-cyan-400/60"
    : "bg-white/5 ring-1 ring-white/10";

  const inner = (
    <>
      <span className="text-[11px] font-bold tabular-nums text-gray-200">{count}×</span>
      <span className="relative inline-flex">
        <ManaSymbol shard={COLOR_SHARD[group.color]} size="md" />
        {group.special && (
          <span
            aria-hidden
            className="absolute -top-1 -right-1 h-2 w-2 rounded-full bg-amber-300 ring-1 ring-slate-900/60"
          />
        )}
      </span>
      {group.sourceName && (
        <span className="max-w-[120px] truncate text-[10px] text-gray-400">
          {group.sourceName}
        </span>
      )}
    </>
  );

  const base = `inline-flex items-center gap-1 rounded-full px-2 py-1 ${ring}`;

  if (!onActivate) {
    return (
      <span title={tooltip} className={base}>
        {inner}
      </span>
    );
  }

  return (
    <button
      type="button"
      onClick={(e) => onActivate(e.shiftKey)}
      aria-pressed={selected}
      title={tooltip}
      className={`${base} transition hover:ring-gray-400`}
    >
      {inner}
    </button>
  );
}
