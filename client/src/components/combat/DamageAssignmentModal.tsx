import { useCallback, useState } from "react";

import type { WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ChoiceOverlay, ConfirmButton } from "../modal/ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";

type AssignCombatDamage = Extract<WaitingFor, { type: "AssignCombatDamage" }>;

export function DamageAssignmentModal({ data }: { data: AssignCombatDamage["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);

  const [amounts, setAmounts] = useState<number[]>(() =>
    data.blockers.map(() => 0),
  );
  const [trampleDamage, setTrampleDamage] = useState(0);

  const blockerTotal = amounts.reduce((acc, n) => acc + n, 0);
  const total = blockerTotal + trampleDamage;
  const remaining = data.total_damage - total;
  // CR 702.19b: Trample requires lethal to every blocker.
  const trampleLethalMet = !data.has_trample ||
    data.blockers.every((b, i) => amounts[i] >= b.lethal_minimum);
  const isValid = total === data.total_damage && trampleLethalMet;

  const getName = (id: number): string =>
    objects?.[String(id)]?.name ?? `Object ${id}`;

  const setAmount = useCallback((index: number, value: number) => {
    setAmounts((prev) => {
      const next = [...prev];
      next[index] = Math.max(0, value);
      return next;
    });
  }, []);

  const handleConfirm = useCallback(() => {
    if (!isValid) return;
    const assignments: [number, number][] = data.blockers.map((b, i) => [
      b.blocker_id,
      amounts[i],
    ]);
    dispatch({
      type: "AssignCombatDamage",
      data: { assignments, trample_damage: trampleDamage },
    });
  }, [dispatch, data.blockers, amounts, trampleDamage, isValid]);

  return (
    <ChoiceOverlay
      title={`Assign ${data.total_damage} Combat Damage`}
      subtitle={`${getName(data.attacker_id)} — Remaining: ${remaining}`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isValid} label="Assign Damage" />}
    >
      <div className="mb-4 space-y-3">
        {data.blockers.map((blocker, i) => {
          const isLethal = amounts[i] >= blocker.lethal_minimum;
          return (
            <div
              key={blocker.blocker_id}
              className="flex items-center justify-between gap-3 rounded-lg bg-gray-800/60 p-3"
            >
              <div className="flex items-center gap-2">
                <span className="text-sm font-medium text-gray-200">
                  {getName(blocker.blocker_id)}
                </span>
                {isLethal && (
                  <span className="rounded bg-red-700/80 px-1.5 py-0.5 text-xs font-bold text-red-100">
                    Lethal
                  </span>
                )}
              </div>
              <div className="flex items-center gap-2">
                <button
                  className={gameButtonClass({ tone: "neutral", size: "xs" })}
                  onClick={() => setAmount(i, amounts[i] - 1)}
                  disabled={amounts[i] <= 0}
                >
                  −
                </button>
                <span className="w-8 text-center text-sm font-bold text-white">
                  {amounts[i]}
                </span>
                <button
                  className={gameButtonClass({ tone: "neutral", size: "xs" })}
                  onClick={() => setAmount(i, amounts[i] + 1)}
                  disabled={remaining <= 0}
                >
                  +
                </button>
              </div>
            </div>
          );
        })}

        {data.has_trample && (
          <div className="flex items-center justify-between gap-3 rounded-lg bg-gray-800/60 p-3 ring-1 ring-amber-600/40">
            <span className="text-sm font-medium text-amber-300">
              Defending Player (Trample)
            </span>
            <div className="flex items-center gap-2">
              <button
                className={gameButtonClass({ tone: "neutral", size: "xs" })}
                onClick={() => setTrampleDamage(Math.max(0, trampleDamage - 1))}
                disabled={trampleDamage <= 0}
              >
                −
              </button>
              <span className="w-8 text-center text-sm font-bold text-amber-200">
                {trampleDamage}
              </span>
              <button
                className={gameButtonClass({ tone: "neutral", size: "xs" })}
                onClick={() => setTrampleDamage(trampleDamage + 1)}
                disabled={remaining <= 0}
              >
                +
              </button>
            </div>
          </div>
        )}
      </div>
    </ChoiceOverlay>
  );
}
