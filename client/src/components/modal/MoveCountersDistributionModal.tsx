import { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  CounterMoveChoice,
  CounterType,
  GameObject,
  ObjectId,
  WaitingFor,
} from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { formatCounterType } from "../../viewmodel/cardProps.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";

type MoveCountersDistribution = Extract<WaitingFor, { type: "MoveCountersDistribution" }>;

function objectLabel(
  objectId: ObjectId,
  objects: Record<ObjectId, GameObject | undefined> | undefined,
): string {
  return objects?.[objectId]?.name ?? `#${objectId}`;
}

function availableCount(available: [CounterType, number][], counterType: CounterType): number {
  return available.find(([ct]) => counterKey(ct) === counterKey(counterType))?.[1] ?? 0;
}

function counterKey(counterType: CounterType): string {
  return typeof counterType === "string" ? counterType : JSON.stringify(counterType);
}

export function MoveCountersDistributionModal({
  data,
}: {
  data: MoveCountersDistribution["data"];
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const availableCounters = useMemo(
    () =>
      data.available
        .filter(([, count]) => count > 0)
        .map(([counterType, count]) => ({
          key: counterKey(counterType),
          counterType,
          count,
          label: formatCounterType(counterType),
        })),
    [data.available],
  );
  const [amounts, setAmounts] = useState<Record<string, Record<ObjectId, number>>>({});

  const totalByType = useMemo(
    () =>
      Object.fromEntries(
        availableCounters.map(({ key }) => [
          key,
          Object.values(amounts[key] ?? {}).reduce((sum, count) => sum + count, 0),
        ]),
      ),
    [amounts, availableCounters],
  );
  const total = useMemo(
    () => Object.values(totalByType).reduce((sum, count) => sum + count, 0),
    [totalByType],
  );
  const max = useMemo(
    () => availableCounters.reduce((sum, { count }) => sum + count, 0),
    [availableCounters],
  );
  const invalid = useMemo(
    () => availableCounters.some(({ key, count }) => (totalByType[key] ?? 0) > count),
    [availableCounters, totalByType],
  );
  const remaining = max - total;

  const setAmount = useCallback(
    (counterType: CounterType, destinationId: ObjectId, value: number) => {
      const key = counterKey(counterType);
      const maxForType = availableCount(data.available, counterType);
      setAmounts((prev) => {
        const currentForType = prev[key] ?? {};
        const withoutDestination = Object.entries(currentForType)
          .filter(([id]) => Number(id) !== destinationId)
          .reduce<Record<ObjectId, number>>((acc, [id, count]) => {
            acc[Number(id)] = count;
            return acc;
          }, {});
        const usedElsewhere = Object.values(withoutDestination).reduce(
          (sum, count) => sum + count,
          0,
        );
        const clamped = Math.max(0, Math.min(value, maxForType - usedElsewhere));
        const nextForType = { ...withoutDestination };
        if (clamped > 0) {
          nextForType[destinationId] = clamped;
        }
        const next = { ...prev };
        if (Object.keys(nextForType).length === 0) {
          delete next[key];
        } else {
          next[key] = nextForType;
        }
        return next;
      });
    },
    [data.available],
  );

  const handleConfirm = useCallback(() => {
    if (invalid) return;
    const selections: CounterMoveChoice[] = availableCounters.flatMap(({ key, counterType }) =>
      Object.entries(amounts[key] ?? {})
        .map(([destinationId, count]) => ({
          destination_id: Number(destinationId),
          counter_type: counterType,
          count,
        }))
        .filter((selection) => selection.count > 0),
    );
    dispatch({ type: "ChooseCounterMoveDistribution", data: { selections } });
  }, [amounts, availableCounters, dispatch, invalid]);

  const counterLabel =
    data.counter_type && availableCounters.length === 1 ? availableCounters[0].label : "counters";

  return (
    <ChoiceOverlay
      title={t("counterMoveDistribution.title", { counter: counterLabel })}
      subtitle={t("counterMoveDistribution.subtitle", {
        source: objectLabel(data.source_id, objects),
        count: max,
        remaining,
      })}
      footer={<ConfirmButton onClick={handleConfirm} disabled={invalid} />}
    >
      <div className="mb-4 space-y-3">
        {availableCounters.map(({ key, counterType, count, label }) => {
          const usedForType = totalByType[key] ?? 0;
          const remainingForType = count - usedForType;
          return (
            <div key={key} className="space-y-2 rounded-lg bg-gray-900/50 p-3">
              <div className="flex items-center justify-between gap-3 text-xs font-semibold uppercase tracking-wide text-gray-400">
                <span>{label}</span>
                <span>{remainingForType}</span>
              </div>
              {data.destinations.map((destinationId) => {
                const amount = amounts[key]?.[destinationId] ?? 0;
                return (
                  <div
                    key={`${key}-${destinationId}`}
                    className="flex items-center justify-between gap-3 rounded-lg bg-gray-800/60 p-3"
                  >
                    <span className="text-sm font-medium text-gray-200">
                      {objectLabel(destinationId, objects)}
                    </span>
                    <div className="flex items-center gap-2">
                      <button
                        className={gameButtonClass({ tone: "neutral", size: "xs" })}
                        onClick={() => setAmount(counterType, destinationId, amount - 1)}
                        disabled={amount <= 0}
                      >
                        -
                      </button>
                      <span className="w-8 text-center text-sm font-bold text-white">
                        {amount}
                      </span>
                      <button
                        className={gameButtonClass({ tone: "neutral", size: "xs" })}
                        onClick={() => setAmount(counterType, destinationId, amount + 1)}
                        disabled={remainingForType <= 0}
                      >
                        +
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
