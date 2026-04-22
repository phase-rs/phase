import { AnimatePresence, motion } from "framer-motion";

import type { ManaCost, ObjectId, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ManaCostSymbols } from "../mana/ManaCostSymbols.tsx";

type CombatTaxPayment = Extract<WaitingFor, { type: "CombatTaxPayment" }>;

/**
 * CR 508.1d + CR 508.1h + CR 509.1c + CR 509.1d: Combat-tax payment prompt.
 * Rendered when one or more declared attackers/blockers are covered by an
 * UnlessPay static (Ghostly Prison, Propaganda, Sphere of Safety, Windborn
 * Muse, etc.). The engine has already aggregated `total_cost` and the
 * per-creature breakdown; the frontend renders the breakdown and dispatches
 * `GameAction::PayCombatTax { accept }`.
 *
 * Per the display-layer mandate, affordability is NOT computed here — the
 * engine's mana-payment pipeline handles invalid payments. If a future
 * engine signal surfaces `can_afford`, wire it to disable the Pay button.
 */

export function CombatTaxModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);

  if (waitingFor?.type !== "CombatTaxPayment") return null;
  if (!canActForWaitingState) return null;

  return <CombatTaxContent data={waitingFor.data as CombatTaxPayment["data"]} />;
}

function CombatTaxContent({ data }: { data: CombatTaxPayment["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);

  const isAttacking = data.context.type === "Attacking";
  const title = isAttacking ? "Pay to Attack" : "Pay to Block";
  const subtitle = isAttacking
    ? "One or more attackers are taxed. Pay the total or remove them from the attack."
    : "One or more blockers are taxed. Pay the total or remove them from the block.";
  const declineLabel = isAttacking
    ? "Decline (remove taxed attackers)"
    : "Decline (remove taxed blockers)";

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
          className="relative z-10 max-h-[calc(100vh_-_2rem_-_env(safe-area-inset-top)_-_env(safe-area-inset-bottom))] w-full max-w-md overflow-y-auto rounded-[16px] lg:rounded-[24px] border border-white/10 bg-[#0b1020]/96 shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md"
          initial={{ scale: 0.95, opacity: 0, y: 10 }}
          animate={{ scale: 1, opacity: 1, y: 0 }}
          exit={{ scale: 0.95, opacity: 0, y: 10 }}
          transition={{ duration: 0.2, ease: "easeOut" }}
        >
          <div className="border-b border-white/10 px-3 py-3 lg:px-5 lg:py-5">
            <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
              Combat Tax
            </div>
            <h2 className="mt-1 text-base font-semibold text-white lg:text-xl">
              {title}
            </h2>
            <p className="mt-1 text-xs text-slate-400 lg:text-sm">{subtitle}</p>
          </div>

          <div className="flex flex-col gap-3 px-3 py-3 lg:px-5 lg:py-5">
            {/* Per-creature breakdown */}
            <div className="flex flex-col gap-1 rounded-[12px] border border-white/5 bg-white/2 px-3 py-2">
              <div className="text-[0.62rem] uppercase tracking-[0.18em] text-slate-500">
                Per-Creature Breakdown
              </div>
              {data.per_creature.map(([objectId, cost]) => (
                <CreatureCostRow
                  key={objectId}
                  objectId={objectId}
                  cost={cost}
                  name={objects?.[objectId]?.name ?? `Creature ${objectId}`}
                />
              ))}
            </div>

            {/* Total */}
            <div className="flex items-center justify-between rounded-[12px] border border-cyan-400/20 bg-cyan-500/8 px-3 py-2">
              <span className="text-sm font-semibold text-cyan-100">Total</span>
              <ManaCostSymbols cost={data.total_cost} />
            </div>

            {/* Actions */}
            <button
              onClick={() =>
                dispatch({ type: "PayCombatTax", data: { accept: true } })
              }
              className="rounded-[16px] border border-cyan-400/30 bg-cyan-500/10 px-4 py-3 text-left transition hover:bg-cyan-500/20 hover:ring-1 hover:ring-cyan-400/40"
            >
              <span className="font-semibold text-white">Pay</span>
              <span className="ml-2">
                <ManaCostSymbols cost={data.total_cost} />
              </span>
            </button>
            <button
              onClick={() =>
                dispatch({ type: "PayCombatTax", data: { accept: false } })
              }
              className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-rose-400/30"
            >
              <span className="font-semibold text-white">{declineLabel}</span>
            </button>
          </div>
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}

function CreatureCostRow({
  cost,
  name,
}: {
  objectId: ObjectId;
  cost: ManaCost;
  name: string;
}) {
  return (
    <div className="flex items-center justify-between py-1 text-sm">
      <span className="truncate text-slate-200">{name}</span>
      <ManaCostSymbols cost={cost} />
    </div>
  );
}
