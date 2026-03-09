import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useEffect, useMemo, useState } from "react";

import type { ManaType } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { ManaBadge } from "./ManaBadge.tsx";
import { ManaSymbol } from "./ManaSymbol.tsx";

const MANA_ORDER: ManaType[] = ["White", "Blue", "Black", "Red", "Green", "Colorless"];

function hasAmbiguousCost(shards: string[]): boolean {
  return shards.some((s) => s.includes("/") || s === "X");
}

/** Calculate max X value based on available mana minus fixed cost requirements */
function computeMaxX(
  shards: string[],
  totalManaInPool: number,
): number {
  const fixedCount = shards.filter((s) => s !== "X").length;
  return Math.max(0, totalManaInPool - fixedCount);
}

export function ManaPaymentUI() {
  const waitingFor = useGameStore((s) => s.waitingFor);
  const gameState = useGameStore((s) => s.gameState);
  const dispatch = useGameStore((s) => s.dispatch);

  const isManaPayment = waitingFor?.type === "ManaPayment";
  const playerId = isManaPayment ? waitingFor.data.player : null;
  const player = playerId != null ? gameState?.players[playerId] : null;

  // Infer the cost being paid from the top stack entry
  const costShards = useMemo(() => {
    if (!gameState || !isManaPayment) return null;
    const stack = gameState.stack;
    if (stack.length === 0) return null;
    const topEntry = stack[stack.length - 1];
    const sourceObj = gameState.objects[topEntry.source_id];
    if (!sourceObj || sourceObj.mana_cost.type !== "Cost") return null;
    return sourceObj.mana_cost.shards;
  }, [gameState, isManaPayment]);

  const cardName = useMemo(() => {
    if (!gameState || !isManaPayment) return null;
    const stack = gameState.stack;
    if (stack.length === 0) return null;
    const topEntry = stack[stack.length - 1];
    return gameState.objects[topEntry.source_id]?.name ?? null;
  }, [gameState, isManaPayment]);

  const isAmbiguous = costShards != null && hasAmbiguousCost(costShards);

  // Auto-pay for simple costs
  useEffect(() => {
    if (!isManaPayment) return;
    // If no shards found or cost is not ambiguous, auto-pay
    if (costShards == null || !hasAmbiguousCost(costShards)) {
      dispatch({ type: "PassPriority" });
    }
  }, [isManaPayment, costShards, dispatch]);

  // Local state for ambiguous cost choices
  const [xValue, setXValue] = useState(0);
  const [phyrexianChoices, setPhyrexianChoices] = useState<Map<number, "mana" | "life">>(
    () => new Map(),
  );
  const [hybridChoices, setHybridChoices] = useState<Map<number, string>>(
    () => new Map(),
  );

  // Reset choices when cost changes
  useEffect(() => {
    setXValue(0);
    setPhyrexianChoices(new Map());
    setHybridChoices(new Map());
  }, [costShards]);

  // Summarize mana pool by color
  const manaPoolSummary = useMemo(() => {
    if (!player) return [];
    const counts: Record<ManaType, number> = {
      White: 0, Blue: 0, Black: 0, Red: 0, Green: 0, Colorless: 0,
    };
    for (const unit of player.mana_pool.mana) {
      counts[unit.color]++;
    }
    return MANA_ORDER.filter((c) => counts[c] > 0).map((c) => ({ color: c, amount: counts[c] }));
  }, [player]);

  const totalManaInPool = useMemo(() => {
    if (!player) return 0;
    return player.mana_pool.mana.length;
  }, [player]);

  const maxX = useMemo(() => {
    if (!costShards) return 0;
    return computeMaxX(costShards, totalManaInPool);
  }, [costShards, totalManaInPool]);

  const togglePhyrexian = useCallback((idx: number) => {
    setPhyrexianChoices((prev) => {
      const next = new Map(prev);
      next.set(idx, next.get(idx) === "life" ? "mana" : "life");
      return next;
    });
  }, []);

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
    dispatch({ type: "PassPriority" });
  }, [dispatch]);

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

  // Don't render if not a mana payment or if auto-paying
  if (!isManaPayment || !player || !isAmbiguous || !costShards) return null;

  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-x-0 bottom-0 z-40 flex justify-center pb-4"
        initial={{ y: 80, opacity: 0 }}
        animate={{ y: 0, opacity: 1 }}
        exit={{ y: 80, opacity: 0 }}
        transition={{ duration: 0.25 }}
      >
        <div className="rounded-xl bg-gray-900/95 p-4 shadow-2xl ring-1 ring-gray-700 min-w-[280px] max-w-[420px]">
          <h3 className="mb-3 text-center text-sm font-semibold text-gray-300">
            Pay Mana Cost
            {cardName && (
              <span className="ml-1 text-gray-400">
                &mdash; {cardName}
              </span>
            )}
          </h3>

          {/* Cost display row */}
          <div className="mb-3 flex items-center justify-center gap-1.5">
            {costShards.map((shard, idx) => (
              <ManaSymbol key={idx} shard={shard} size="lg" />
            ))}
          </div>

          {/* X cost slider */}
          {costShards.some((s) => s === "X") && (
            <div className="mb-3 px-2">
              <label className="flex items-center gap-2 text-xs text-gray-400">
                <span className="shrink-0">X = {xValue}</span>
                <input
                  type="range"
                  min={0}
                  max={maxX}
                  value={xValue}
                  onChange={(e) => setXValue(Number(e.target.value))}
                  className="h-1.5 w-full cursor-pointer appearance-none rounded-full bg-gray-700 accent-cyan-500"
                />
                <span className="shrink-0 text-gray-500">
                  Total: {costShards.filter((s) => s !== "X").length + xValue}
                </span>
              </label>
            </div>
          )}

          {/* Phyrexian toggles */}
          {costShards.some((s) => s.endsWith("/P")) && (
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
          {costShards.some(
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

          {/* Current mana pool */}
          <div className="mb-3 flex items-center justify-center gap-2">
            <span className="text-xs text-gray-500">Pool:</span>
            {manaPoolSummary.length > 0 ? (
              manaPoolSummary.map(({ color, amount }) => (
                <ManaBadge key={color} color={color} amount={amount} />
              ))
            ) : (
              <span className="text-xs text-gray-600">Empty</span>
            )}
          </div>

          {/* Confirm / Cancel buttons */}
          <div className="flex justify-center gap-3">
            <button
              onClick={handlePay}
              className={gameButtonClass({ tone: "emerald", size: "md" })}
            >
              Pay
            </button>
            <button
              onClick={handleCancel}
              className="rounded-lg bg-gray-700 px-4 py-1.5 text-sm font-semibold text-gray-200 transition hover:bg-gray-600"
            >
              Cancel
            </button>
          </div>
        </div>
      </motion.div>
    </AnimatePresence>
  );
}
