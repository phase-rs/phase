import { type ReactNode, useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { HalfFraction, ObjectId, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { formatHalves, fromHalves, toHalves } from "../../viewmodel/halfPoints.ts";
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
 * Amounts in these modals are held in halves so an Unhinged half-point total
 * (Vile Bile's 2½ power) can be divided exactly. A prompt whose total and
 * lethal thresholds are all whole steps by 1 (two halves); a half-point prompt
 * steps by ½.
 */
function stepHalves(halfPointPrompt: boolean): number {
  return halfPointPrompt ? 1 : 2;
}

/**
 * Split per-target amounts in halves into the engine's whole `assignments` and
 * its `assignments_fraction` remainders (only the targets that carry a half).
 */
function splitAssignments(
  ids: ObjectId[],
  amounts: number[],
): { assignments: [ObjectId, number][]; assignments_fraction: [ObjectId, HalfFraction][] } {
  const assignments: [ObjectId, number][] = [];
  const assignmentsFraction: [ObjectId, HalfFraction][] = [];
  ids.forEach((id, i) => {
    const { whole, fraction } = fromHalves(amounts[i]);
    assignments.push([id, whole]);
    if (fraction === "Half") assignmentsFraction.push([id, fraction]);
  });
  return { assignments, assignments_fraction: assignmentsFraction };
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
  /** The row's assigned amount, in halves. */
  value: number;
  valueClassName?: string;
  ringClassName?: string;
  onDecrement: () => void;
  onIncrement: () => void;
  decrementDisabled: boolean;
  incrementDisabled: boolean;
  /** The undivided amount, in halves. */
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
          {formatHalves(value)}
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
          {t("combat.assignRemainingButton", { amount: formatHalves(remaining) })}
        </button>
      </div>
    </div>
  );
}

export function DamageAssignmentModal({ data }: { data: AssignCombatDamage["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const { getName, getStats } = useObjectNamer();

  // Every amount below is in halves (see `stepHalves`).
  const totalHalves = toHalves(data.total_damage, data.total_damage_fraction);
  const lethalHalves = data.blockers.map((b) => toHalves(b.lethal_minimum, b.lethal_minimum_fraction));
  const step = stepHalves(
    totalHalves % 2 !== 0 || lethalHalves.some((halves) => halves % 2 !== 0),
  );
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
  const remaining = totalHalves - total;
  // CR 702.19b: Lethal-to-all-blockers is a precondition only for assigning
  // excess to the defending player/planeswalker, not an unconditional constraint.
  // When trampleDamage and controllerDamage are both 0 the player is freely
  // dividing all damage among blockers, so any split is legal.
  const trampleLethalMet = data.trample == null ||
    (trampleDamage === 0 && controllerDamage === 0) ||
    lethalHalves.every((lethal, i) => amounts[i] >= lethal);
  // CR 702.19c: Must assign at least PW loyalty before controller spillover.
  const loyaltyMet = !isOverPw || controllerDamage === 0 ||
    trampleDamage >= toHalves(data.pw_loyalty ?? 0);
  const isValid = total === totalHalves && trampleLethalMet && loyaltyMet;
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
    const { assignments, assignments_fraction } = splitAssignments(
      data.blockers.map((b) => b.blocker_id),
      amounts,
    );
    const trample = fromHalves(trampleDamage);
    const controller = fromHalves(controllerDamage);
    dispatch({
      type: "AssignCombatDamage",
      data: {
        assignments,
        trample_damage: trample.whole,
        controller_damage: controller.whole,
        // Half-point remainders are omitted when zero, matching the engine's wire shape.
        ...(assignments_fraction.length > 0 ? { assignments_fraction } : {}),
        ...(trample.fraction === "Half" ? { trample_damage_fraction: trample.fraction } : {}),
        ...(controller.fraction === "Half" ? { controller_damage_fraction: controller.fraction } : {}),
      },
    });
  }, [dispatch, data.blockers, amounts, trampleDamage, controllerDamage, isValid]);

  if (submitted) return null;

  return (
    <ChoiceOverlay
      title={t("combat.assignDamageTitle", { amount: formatHalves(totalHalves) })}
      subtitle={t("combat.assignDamageSubtitle", { name: getName(data.attacker_id), remaining: formatHalves(remaining) })}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isValid} label={t("combat.assignDamageButton")} />}
    >
      <div className="mb-4 space-y-3">
        {data.blockers.map((blocker, i) => {
          const isLethal = amounts[i] >= lethalHalves[i];
          const stats = getStats(blocker.blocker_id);
          const name = getName(blocker.blocker_id);
          return (
            <StepperRow
              key={blocker.blocker_id}
              value={amounts[i]}
              onDecrement={() => setAmount(i, amounts[i] - step)}
              onIncrement={() => setAmount(i, amounts[i] + step)}
              decrementDisabled={amounts[i] <= 0}
              incrementDisabled={remaining < step}
              remaining={remaining}
              onAssignRemaining={() => setAmount(i, amounts[i] + remaining)}
              assignRemainingLabel={t("combat.assignRemainingTo", { count: remaining / 2, target: name })}
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
                {t("combat.lethalLabel", { amount: formatHalves(lethalHalves[i]) })}
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
            onDecrement={() => setTrampleDamage(Math.max(0, trampleDamage - step))}
            onIncrement={() => setTrampleDamage(trampleDamage + step)}
            decrementDisabled={trampleDamage <= 0}
            incrementDisabled={remaining < step}
            remaining={remaining}
            onAssignRemaining={() => setTrampleDamage(trampleDamage + remaining)}
            assignRemainingLabel={t("combat.assignRemainingTo", { count: remaining / 2, target: trampleLabel })}
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
            onDecrement={() => setControllerDamage(Math.max(0, controllerDamage - step))}
            onIncrement={() => setControllerDamage(controllerDamage + step)}
            decrementDisabled={controllerDamage <= 0}
            incrementDisabled={remaining < step}
            remaining={remaining}
            onAssignRemaining={() => setControllerDamage(controllerDamage + remaining)}
            assignRemainingLabel={t("combat.assignRemainingTo", { count: remaining / 2, target: controllerLabel })}
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

  // Every amount below is in halves (see `stepHalves`).
  const totalHalves = toHalves(data.total_damage, data.total_damage_fraction);
  const step = stepHalves(totalHalves % 2 !== 0);
  const [amounts, setAmounts] = useState<number[]>(() => data.attackers.map(() => 0));
  const [submitted, setSubmitted] = useState(false);
  const submittedRef = useRef(false);

  const total = amounts.reduce((acc, n) => acc + n, 0);
  const remaining = totalHalves - total;
  // CR 510.1e: the only legality constraint on blocker division is that the
  // assigned total equals the blocker's power (no per-target lethal minimum).
  const isValid = total === totalHalves;

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
    const { assignments, assignments_fraction } = splitAssignments(data.attackers, amounts);
    dispatch({
      type: "AssignBlockerDamage",
      data: { assignments, ...(assignments_fraction.length > 0 ? { assignments_fraction } : {}) },
    });
  }, [dispatch, data.attackers, amounts, isValid]);

  if (submitted) return null;

  return (
    <ChoiceOverlay
      title={t("combat.assignDamageTitle", { amount: formatHalves(totalHalves) })}
      subtitle={t("combat.assignDamageSubtitle", { name: getName(data.blocker_id), remaining: formatHalves(remaining) })}
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
              onDecrement={() => setAmount(i, amounts[i] - step)}
              onIncrement={() => setAmount(i, amounts[i] + step)}
              decrementDisabled={amounts[i] <= 0}
              incrementDisabled={remaining < step}
              remaining={remaining}
              onAssignRemaining={() => setAmount(i, amounts[i] + remaining)}
              assignRemainingLabel={t("combat.assignRemainingTo", { count: remaining / 2, target: name })}
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
