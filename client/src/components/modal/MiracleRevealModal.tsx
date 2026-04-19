import { AnimatePresence, motion } from "framer-motion";

import type { GameAction, ManaCost, WaitingFor } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { SHARD_ABBREVIATION } from "../../viewmodel/costLabel.ts";
import { ManaSymbol } from "../mana/ManaSymbol.tsx";

type MiracleReveal = Extract<WaitingFor, { type: "MiracleReveal" }>;
type MiracleCastOffer = Extract<WaitingFor, { type: "MiracleCastOffer" }>;

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

export function MiracleRevealModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  if (!canActForWaitingState) return null;

  if (waitingFor?.type === "MiracleReveal") {
    const data = waitingFor.data as MiracleReveal["data"];
    return (
      <MiracleRevealContent
        objectId={data.object_id}
        cost={data.cost}
        dispatch={dispatch}
        phase="reveal"
      />
    );
  }

  if (waitingFor?.type === "MiracleCastOffer") {
    const data = waitingFor.data as MiracleCastOffer["data"];
    return (
      <MiracleRevealContent
        objectId={data.object_id}
        cost={data.cost}
        dispatch={dispatch}
        phase="cast"
      />
    );
  }

  return null;
}

function MiracleRevealContent({
  objectId,
  cost,
  dispatch,
  phase,
}: {
  objectId: number;
  cost: ManaCost;
  dispatch: (action: GameAction) => Promise<unknown>;
  phase: "reveal" | "cast";
}) {
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);

  if (!obj) return null;

  const cardName = obj.name;
  const cardId = obj.card_id;

  const isReveal = phase === "reveal";

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
            <div className="text-[0.68rem] uppercase tracking-[0.22em] text-amber-300/80">
              Miracle
            </div>
            <h2 className="mt-1 text-base font-semibold text-white lg:text-xl">
              {isReveal ? `Reveal ${cardName}?` : `Cast ${cardName}?`}
            </h2>
            <p className="mt-1 text-xs text-slate-400 lg:text-sm">
              {isReveal
                ? "You may reveal this card to cast it for its miracle cost."
                : "You may cast this card for its miracle cost."}
            </p>
          </div>
          <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
            <button
              onClick={() =>
                dispatch({
                  type: "CastSpellAsMiracle",
                  data: { object_id: objectId, card_id: cardId },
                })
              }
              className="rounded-[16px] border border-amber-400/20 bg-amber-400/10 px-4 py-3 text-left transition hover:bg-amber-400/20 hover:ring-1 hover:ring-amber-400/40"
            >
              <span className="font-semibold text-white">
                {isReveal ? "Reveal" : "Cast"}
              </span>
              <span className="ml-2">
                <ManaCostSymbols cost={cost} />
              </span>
            </button>
            <button
              onClick={() =>
                dispatch({
                  type: "DecideOptionalEffect",
                  data: { accept: false },
                })
              }
              className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-white/20"
            >
              <span className="font-semibold text-white">Decline</span>
            </button>
          </div>
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}
