import { AnimatePresence, motion } from "framer-motion";

import type { GameAction, ManaCost, WaitingFor } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { SHARD_ABBREVIATION } from "../../viewmodel/costLabel.ts";
import { ManaSymbol } from "../mana/ManaSymbol.tsx";

type WarpCostChoice = Extract<WaitingFor, { type: "WarpCostChoice" }>;

function ManaCostSymbols({ cost }: { cost: ManaCost }) {
  if (cost.type === "NoCost" || cost.type === "SelfManaCost")
    return <span className="text-slate-500">Free</span>;
  const symbols: string[] = [];
  if (cost.generic > 0) symbols.push(String(cost.generic));
  for (const shard of cost.shards) {
    symbols.push(SHARD_ABBREVIATION[shard] ?? shard);
  }
  if (symbols.length === 0) symbols.push("0");
  return (
    <span className="inline-flex items-center gap-0.5">
      {symbols.map((s, i) => (
        <ManaSymbol key={i} shard={s} size="sm" />
      ))}
    </span>
  );
}

export function WarpCostModal() {
  const playerId = usePlayerId();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  if (waitingFor?.type !== "WarpCostChoice") return null;
  if (waitingFor.data.player !== playerId) return null;

  const data = waitingFor.data as WarpCostChoice["data"];

  return (
    <WarpCostContent
      objectId={data.object_id}
      normalCost={data.normal_cost}
      warpCost={data.warp_cost}
      dispatch={dispatch}
    />
  );
}

function WarpCostContent({
  objectId,
  normalCost,
  warpCost,
  dispatch,
}: {
  objectId: number;
  normalCost: ManaCost;
  warpCost: ManaCost;
  dispatch: (action: GameAction) => Promise<unknown>;
}) {
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);

  if (!obj) return null;

  const cardName = obj.name;

  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-0 z-50 flex items-center justify-center px-2 py-2 lg:px-4 lg:py-6"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.2 }}
      >
        <div className="absolute inset-0 bg-black/60" />

        <motion.div
          className="relative z-10 w-full max-w-sm overflow-hidden rounded-[16px] lg:rounded-[24px] border border-white/10 bg-[#0b1020]/96 shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md"
          initial={{ scale: 0.95, opacity: 0, y: 10 }}
          animate={{ scale: 1, opacity: 1, y: 0 }}
          exit={{ scale: 0.95, opacity: 0, y: 10 }}
          transition={{ duration: 0.2, ease: "easeOut" }}
        >
          <div className="border-b border-white/10 px-3 py-3 lg:px-5 lg:py-5">
            <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
              Warp
            </div>
            <h2 className="mt-1 text-base font-semibold text-white lg:text-xl">Choose Casting Cost</h2>
            <p className="mt-1 text-xs text-slate-400 lg:text-sm">
              Cast {cardName} normally or use its Warp cost.
            </p>
          </div>
          <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
            <button
              onClick={() =>
                dispatch({ type: "ChooseWarpCost", data: { use_warp: false } })
              }
              className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
            >
              <span className="font-semibold text-white">Cast Normally</span>
              <span className="ml-2"><ManaCostSymbols cost={normalCost} /></span>
            </button>
            <button
              onClick={() =>
                dispatch({ type: "ChooseWarpCost", data: { use_warp: true } })
              }
              className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-amber-400/30"
            >
              <span className="font-semibold text-white">Cast with Warp</span>
              <span className="ml-2"><ManaCostSymbols cost={warpCost} /></span>
              <span className="ml-1 text-xs text-slate-400">(exiles at end step)</span>
            </button>
          </div>
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}
