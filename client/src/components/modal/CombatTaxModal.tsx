import type { ManaCost, ObjectId, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ManaCostSymbols } from "../mana/ManaCostSymbols.tsx";
import { DialogShell } from "./DialogShell.tsx";

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
    <DialogShell
      eyebrow="Combat Tax"
      title={title}
      subtitle={subtitle}
      size="md"
      scrollable
    >
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
    </DialogShell>
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
