import { type ReactNode, useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ChoiceOverlay, ConfirmButton } from "../modal/ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";

type AssignCombatDamage = Extract<WaitingFor, { type: "AssignCombatDamage" }>;
type AssignBlockerDamage = Extract<WaitingFor, { type: "AssignBlockerDamage" }>;

/** Resolves object ids to display name + P/T from engine-provided state. */
function useObjectNamer() {
  const objects = useGameStore((s) => s.gameState?.objects);
  const getName = (id: number): string => objects?.[String(id)]?.name ?? `Object ${id}`;
  const getStats = (id: number): string => {
    const obj = objects?.[String(id)];
    if (obj?.power == null || obj?.toughness == null) return "";
    return `${obj.power}/${obj.toughness}`;
  };
  return { getName, getStats };
}

/**
 * One damage-division row: a labelled target with −/value/+ steppers and an
 * "assign the rest here" shortcut. The shortcut is input arithmetic only
 * (`row += remaining`); CR 702.19b legality stays with the engine validator
 * and the modal's existing advisory gate.
 */
function StepperRow({
  children,
  value,
  valueClassName,
  ringClassName,
  onDecrement,
  onIncrement,
  decrementDisabled,
  incrementDisabled,
  remaining,
  onAssignRemaining,
  assignRemainingLabel,
}: {
  children: ReactNode;
  value: number;
  valueClassName?: string;
  ringClassName?: string;
  onDecrement: () => void;
  onIncrement: () => void;
  decrementDisabled: boolean;
  incrementDisabled: boolean;
  remaining: number;
  onAssignRemaining: () => void;
  assignRemainingLabel: string;
}) {
  const { t } = useTranslation("game");
  return (
    <div
      className={`flex items-center justify-between gap-3 rounded-lg bg-gray-800/60 p-3${ringClassName ? ` ${ringClassName}` : ""}`}
    >
      <div className="flex items-center gap-2">{children}</div>
      <div className="flex items-center gap-2">
        <button
          className={gameButtonClass({ tone: "neutral", size: "xs" })}
          onClick={onDecrement}
          disabled={decrementDisabled}
        >
          −
        </button>
        <span className={`w-8 text-center text-sm font-bold ${valueClassName ?? "text-white"}`}>
          {value}
        </span>
        <button
          className={gameButtonClass({ tone: "neutral", size: "xs" })}
          onClick={onIncrement}
          disabled={incrementDisabled}
        >
          +
        </button>
        <button
          className={gameButtonClass({ tone: "neutral", size: "xs" })}
          onClick={onAssignRemaining}
          disabled={remaining <= 0}
          aria-label={assignRemainingLabel}
          title={assignRemainingLabel}
        >
          {t("combat.assignRemainingButton", { amount: remaining })}
        </button>
      </div>
    </div>
  );
}

export function DamageAssignmentModal({ data }: { data: AssignCombatDamage["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const { getName, getStats } = useObjectNamer();

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
  const trampleLabel = isOverPw
    ? t("combat.planeswalkerLoyalty", { loyalty: data.pw_loyalty ?? 0 })
    : t("combat.defendingPlayerTrample");
  const controllerLabel = t("combat.pwControllerTrample");

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
          const name = getName(blocker.blocker_id);
          return (
            <StepperRow
              key={blocker.blocker_id}
              value={amounts[i]}
              onDecrement={() => setAmount(i, amounts[i] - 1)}
              onIncrement={() => setAmount(i, amounts[i] + 1)}
              decrementDisabled={amounts[i] <= 0}
              incrementDisabled={remaining <= 0}
              remaining={remaining}
              onAssignRemaining={() => setAmount(i, amounts[i] + remaining)}
              assignRemainingLabel={t("combat.assignRemainingTo", { count: remaining, target: name })}
            >
              <span className="text-sm font-medium text-gray-200">
                {name}
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
            </StepperRow>
          );
        })}

        {data.trample != null && (
          <StepperRow
            value={trampleDamage}
            valueClassName="text-amber-200"
            ringClassName="ring-1 ring-amber-600/40"
            onDecrement={() => setTrampleDamage(Math.max(0, trampleDamage - 1))}
            onIncrement={() => setTrampleDamage(trampleDamage + 1)}
            decrementDisabled={trampleDamage <= 0}
            incrementDisabled={remaining <= 0}
            remaining={remaining}
            onAssignRemaining={() => setTrampleDamage(trampleDamage + remaining)}
            assignRemainingLabel={t("combat.assignRemainingTo", { count: remaining, target: trampleLabel })}
          >
            <span className="text-sm font-medium text-amber-300">
              {trampleLabel}
            </span>
          </StepperRow>
        )}

        {isOverPw && (
          <StepperRow
            value={controllerDamage}
            valueClassName="text-purple-200"
            ringClassName="ring-1 ring-purple-600/40"
            onDecrement={() => setControllerDamage(Math.max(0, controllerDamage - 1))}
            onIncrement={() => setControllerDamage(controllerDamage + 1)}
            decrementDisabled={controllerDamage <= 0}
            incrementDisabled={remaining <= 0}
            remaining={remaining}
            onAssignRemaining={() => setControllerDamage(controllerDamage + remaining)}
            assignRemainingLabel={t("combat.assignRemainingTo", { count: remaining, target: controllerLabel })}
          >
            <span className="text-sm font-medium text-purple-300">
              {controllerLabel}
            </span>
          </StepperRow>
        )}
      </div>
    </ChoiceOverlay>
  );
}

/**
 * CR 510.1d + CR 702.22k: the active player divides a blocking creature's
 * combat damage among the attackers it's blocking (free division — no lethal
 * ordering, no trample). Surfaced when the blocker is blocking a banded
 * attacker. Reuses the same division chrome as the attacker modal.
 */
export function BlockerDamageAssignmentModal({ data }: { data: AssignBlockerDamage["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const { getName, getStats } = useObjectNamer();

  const [amounts, setAmounts] = useState<number[]>(() => data.attackers.map(() => 0));
  const [submitted, setSubmitted] = useState(false);
  const submittedRef = useRef(false);

  const total = amounts.reduce((acc, n) => acc + n, 0);
  const remaining = data.total_damage - total;
  // CR 510.1e: the only legality constraint on blocker division is that the
  // assigned total equals the blocker's power (no per-target lethal minimum).
  const isValid = total === data.total_damage;

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
    const assignments: [number, number][] = data.attackers.map((id, i) => [id, amounts[i]]);
    dispatch({ type: "AssignBlockerDamage", data: { assignments } });
  }, [dispatch, data.attackers, amounts, isValid]);

  if (submitted) return null;

  return (
    <ChoiceOverlay
      title={t("combat.assignDamageTitle", { amount: data.total_damage })}
      subtitle={t("combat.assignDamageSubtitle", { name: getName(data.blocker_id), remaining })}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isValid} label={t("combat.assignDamageButton")} />}
    >
      <div className="mb-4 space-y-3">
        {data.attackers.map((attackerId, i) => {
          const stats = getStats(attackerId);
          const name = getName(attackerId);
          return (
            <StepperRow
              key={attackerId}
              value={amounts[i]}
              onDecrement={() => setAmount(i, amounts[i] - 1)}
              onIncrement={() => setAmount(i, amounts[i] + 1)}
              decrementDisabled={amounts[i] <= 0}
              incrementDisabled={remaining <= 0}
              remaining={remaining}
              onAssignRemaining={() => setAmount(i, amounts[i] + remaining)}
              assignRemainingLabel={t("combat.assignRemainingTo", { count: remaining, target: name })}
            >
              <span className="text-sm font-medium text-gray-200">
                {name}
              </span>
              {stats && (
                <span className="rounded bg-gray-700/80 px-1.5 py-0.5 text-xs font-medium text-gray-400">
                  {stats}
                </span>
              )}
            </StepperRow>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
