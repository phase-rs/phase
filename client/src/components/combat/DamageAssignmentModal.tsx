import { useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ChoiceOverlay, ConfirmButton } from "../modal/ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";

type AssignCombatDamage = Extract<WaitingFor, { type: "AssignCombatDamage" }>;

export function DamageAssignmentModal({ data }: { data: AssignCombatDamage["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);

  const [amounts, setAmounts] = useState<number[]>(() =>
    data.blockers.map(() => 0),
  );
  const [trampleDamage, setTrampleDamage] = useState(0);
  const [controllerDamage, setControllerDamage] = useState(0);
  const [submitted, setSubmitted] = useState(false);
  const submittedRef = useRef(false);

  const isOverPw = data.trample === "OverPlaneswalkers" && data.pw_controller != null;
  const blockerTotal = amounts.reduce((acc, n) => acc + n, 0);
  const total = blockerTotal + trampleDamage + controllerDamage;
  const remaining = data.total_damage - total;
  // CR 702.19b: Lethal-to-all-blockers is a precondition only for assigning
  // excess to the defending player/planeswalker, not an unconditional constraint.
  // When trampleDamage and controllerDamage are both 0 the player is freely
  // dividing all damage among blockers, so any split is legal.
  const trampleLethalMet = data.trample == null ||
    (trampleDamage === 0 && controllerDamage === 0) ||
    data.blockers.every((b, i) => amounts[i] >= b.lethal_minimum);
  // CR 702.19c: Must assign at least PW loyalty before controller spillover.
  const loyaltyMet = !isOverPw || controllerDamage === 0 ||
    trampleDamage >= (data.pw_loyalty ?? 0);
  const isValid = total === data.total_damage && trampleLethalMet && loyaltyMet;

  const getObject = (id: number) => objects?.[String(id)];
  const getName = (id: number): string =>
    getObject(id)?.name ?? `Object ${id}`;
  const getStats = (id: number): string => {
    const obj = getObject(id);
    if (obj?.power == null || obj?.toughness == null) return "";
    return `${obj.power}/${obj.toughness}`;
  };

  const setAmount = useCallback((index: number, value: number) => {
    setAmounts((prev) => {
      const next = [...prev];
      next[index] = Math.max(0, value);
      return next;
    });
  }, []);

  const handleConfirm = useCallback(() => {
    if (!isValid || submittedRef.current) return;
    submittedRef.current = true;
    setSubmitted(true);
    const assignments: [number, number][] = data.blockers.map((b, i) => [
      b.blocker_id,
      amounts[i],
    ]);
    dispatch({
      type: "AssignCombatDamage",
      data: { assignments, trample_damage: trampleDamage, controller_damage: controllerDamage },
    });
  }, [dispatch, data.blockers, amounts, trampleDamage, controllerDamage, isValid]);

  if (submitted) return null;

  return (
    <ChoiceOverlay
      title={t("combat.assignDamageTitle", { amount: data.total_damage })}
      subtitle={t("combat.assignDamageSubtitle", { name: getName(data.attacker_id), remaining })}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isValid} label={t("combat.assignDamageButton")} />}
    >
      <div className="mb-4 space-y-3">
        {data.blockers.map((blocker, i) => {
          const isLethal = amounts[i] >= blocker.lethal_minimum;
          const stats = getStats(blocker.blocker_id);
          return (
            <div
              key={blocker.blocker_id}
              className="flex items-center justify-between gap-3 rounded-lg bg-gray-800/60 p-3"
            >
              <div className="flex items-center gap-2">
                <span className="text-sm font-medium text-gray-200">
                  {getName(blocker.blocker_id)}
                </span>
                {stats && (
                  <span className="rounded bg-gray-700/80 px-1.5 py-0.5 text-xs font-medium text-gray-400">
                    {stats}
                  </span>
                )}
                <span className="text-xs text-gray-500">
                  {t("combat.lethalLabel", { amount: blocker.lethal_minimum })}
                </span>
                {isLethal && (
                  <span className="rounded bg-red-700/80 px-1.5 py-0.5 text-xs font-bold text-red-100">
                    {t("combat.lethalBadge")}
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

        {data.trample != null && (
          <div className="flex items-center justify-between gap-3 rounded-lg bg-gray-800/60 p-3 ring-1 ring-amber-600/40">
            <span className="text-sm font-medium text-amber-300">
              {isOverPw ? t("combat.planeswalkerLoyalty", { loyalty: data.pw_loyalty ?? 0 }) : t("combat.defendingPlayerTrample")}
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

        {isOverPw && (
          <div className="flex items-center justify-between gap-3 rounded-lg bg-gray-800/60 p-3 ring-1 ring-purple-600/40">
            <span className="text-sm font-medium text-purple-300">
              {t("combat.pwControllerTrample")}
            </span>
            <div className="flex items-center gap-2">
              <button
                className={gameButtonClass({ tone: "neutral", size: "xs" })}
                onClick={() => setControllerDamage(Math.max(0, controllerDamage - 1))}
                disabled={controllerDamage <= 0}
              >
                −
              </button>
              <span className="w-8 text-center text-sm font-bold text-purple-200">
                {controllerDamage}
              </span>
              <button
                className={gameButtonClass({ tone: "neutral", size: "xs" })}
                onClick={() => setControllerDamage(controllerDamage + 1)}
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
