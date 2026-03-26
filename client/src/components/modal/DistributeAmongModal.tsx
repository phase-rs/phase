import { useCallback, useState } from "react";

import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { DistributionUnit, GameObject, TargetRef, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";

type DistributeAmong = Extract<WaitingFor, { type: "DistributeAmong" }>;

function targetLabel(target: TargetRef, objects: Record<string, GameObject> | undefined): string {
  if ("Object" in target) {
    return objects?.[String(target.Object)]?.name ?? `Object ${target.Object}`;
  }
  return `Player ${target.Player}`;
}

function unitLabel(unit: DistributionUnit): string {
  if (unit.type === "Counters") return `${unit.data} counter`;
  return unit.type === "Damage" ? "damage" : "life";
}

export function DistributeAmongModal({ data }: { data: DistributeAmong["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);

  // One amount per target index; initialize with 0 each.
  const [amounts, setAmounts] = useState<number[]>(() =>
    data.targets.map(() => 0),
  );

  const total = amounts.reduce((acc, n) => acc + n, 0);
  const remaining = data.total - total;
  const isValid = total === data.total && amounts.every((n) => n >= 1);

  const setAmount = useCallback((index: number, value: number) => {
    setAmounts((prev) => {
      const next = [...prev];
      next[index] = Math.max(0, value);
      return next;
    });
  }, []);

  const handleConfirm = useCallback(() => {
    if (!isValid) return;
    const distribution: [TargetRef, number][] = data.targets.map((target, i) => [
      target,
      amounts[i],
    ]);
    dispatch({ type: "DistributeAmong", data: { distribution } });
  }, [dispatch, data.targets, amounts, isValid]);

  const label = unitLabel(data.unit);

  return (
    <ChoiceOverlay
      title={`Distribute ${data.total} ${label}`}
      subtitle={`Assign at least 1 ${label} to each target. Remaining: ${remaining}`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isValid} label="Confirm" />}
    >
      <div className="mb-4 space-y-3">
        {data.targets.map((target, i) => (
          <div
            key={i}
            className="flex items-center justify-between gap-3 rounded-lg bg-gray-800/60 p-3"
          >
            <span className="text-sm font-medium text-gray-200">
              {targetLabel(target, objects)}
            </span>
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
        ))}
      </div>
    </ChoiceOverlay>
  );
}
