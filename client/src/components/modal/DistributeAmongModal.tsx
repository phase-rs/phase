import { useCallback, useState } from "react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";

import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { DistributionUnit, TargetRef, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { targetLabel } from "./targetRef.ts";
import { formatCounterType } from "../../viewmodel/cardProps.ts";

type DistributeAmong = Extract<WaitingFor, { type: "DistributeAmong" }>;

function unitLabel(unit: DistributionUnit, t: TFunction<"game">): string {
  if (unit.type === "Counters")
    return t("distributeAmong.unitCounter", { counter: formatCounterType(unit.data) });
  return unit.type === "Damage"
    ? t("distributeAmong.unitDamage")
    : t("distributeAmong.unitLife");
}

export function DistributeAmongModal({ data }: { data: DistributeAmong["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  // One amount per target index; initialize with 0 each.
  const [amounts, setAmounts] = useState<number[]>(() =>
    data.targets.map(() => 0),
  );

  // CR 608.2d: a resolution-time division is among ANY number of the
  // candidates (unchosen ones stay at 0); an announce-time division (CR 601.2d)
  // gives every target at least one. This only mirrors the engine-provided
  // scope for the confirm gate — the engine validator is authoritative.
  const anyNumber = data.scope?.type === "ResolutionCandidates";
  const total = amounts.reduce((acc, n) => acc + n, 0);
  const remaining = data.total - total;
  const isValid =
    total === data.total &&
    (anyNumber ? amounts.some((n) => n > 0) : amounts.every((n) => n >= 1));

  const setAmount = useCallback((index: number, value: number) => {
    setAmounts((prev) => {
      const next = [...prev];
      next[index] = Math.max(0, value);
      return next;
    });
  }, []);

  const handleConfirm = useCallback(() => {
    if (!isValid) return;
    const assigned: [TargetRef, number][] = data.targets.map((target, i) => [
      target,
      amounts[i],
    ]);
    const distribution = anyNumber ? assigned.filter(([, n]) => n > 0) : assigned;
    dispatch({ type: "DistributeAmong", data: { distribution } });
  }, [dispatch, data.targets, amounts, isValid, anyNumber]);

  const label = unitLabel(data.unit, t);

  return (
    <ChoiceOverlay
      title={t("distributeAmong.title", { total: data.total, unit: label })}
      subtitle={t(
        anyNumber ? "distributeAmong.subtitleAnyNumber" : "distributeAmong.subtitle",
        { unit: label, remaining },
      )}
      maxWidthClassName="max-w-xl"
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isValid} />}
    >
      <div className="mb-4 space-y-3">
        {data.targets.map((target, i) => (
          <div
            key={i}
            {...("Object" in target ? hoverProps(target.Object) : undefined)}
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
