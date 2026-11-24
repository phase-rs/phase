import { useCallback, useRef, useState } from "react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";

import type { WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ChoiceOverlay, ConfirmButton } from "../modal/ChoiceOverlay.tsx";
import { StepperRow, type StepperShortcut } from "../modal/StepperRow.tsx";
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
 * The shortcuts every damage-division row carries: "assign the rest here" and
 * "clear". Both are input arithmetic only (`row += remaining`, `row = 0`);
 * CR 702.19b legality stays with the engine validator and the modal's existing
 * advisory gate.
 */
function damageShortcuts(
  t: TFunction<"game">,
  target: string,
  value: number,
  remaining: number,
  setValue: (value: number) => void,
): StepperShortcut[] {
  return [
    {
      key: "remaining",
      label: t("combat.assignRemainingButton", { amount: remaining }),
      ariaLabel: t("combat.assignRemainingTo", { count: remaining, target }),
      onClick: () => setValue(value + remaining),
      disabled: remaining <= 0,
    },
    {
      key: "clear",
      label: t("combat.clearButton"),
      ariaLabel: t("combat.clearDamageFor", { target }),
      onClick: () => setValue(0),
      disabled: value <= 0,
    },
  ];
}

/**
 * The one-tap draft: each blocker's engine-provided `lethal_minimum` in prompt
 * order, as far as the damage reaches, then the excess wherever engine-provided
 * data name a recipient for it. Input arithmetic only — it never decides what
 * is lethal or what the creature is attacking, and an excess with no known
 * recipient is left for the player to place.
 */
function lethalPreset(data: AssignCombatDamage["data"], hasAttackTarget: boolean) {
  let excess = data.total_damage;
  const amounts = data.blockers.map(({ lethal_minimum }) => {
    const amount = Math.min(lethal_minimum, excess);
    excess -= amount;
    return amount;
  });

  let trampleDamage = 0;
  let controllerDamage = 0;
  if (data.trample === "OverPlaneswalkers" && data.attack_target.type === "Planeswalker") {
    // CR 702.19c: the prompt carries the loyalty to assign before any spillover
    // to the planeswalker's controller. CR 702.19e: once the planeswalker has
    // left combat the prompt carries no loyalty and names the original defender.
    if (data.pw_controller != null) {
      trampleDamage = Math.min(excess, data.pw_loyalty ?? 0);
      controllerDamage = excess - trampleDamage;
    }
  } else if (data.trample != null && hasAttackTarget) {
    // CR 702.19b + CR 506.4c: excess goes to what the creature is attacking
    // only while the engine still reports that attack relation.
    trampleDamage = excess;
  }

  return { amounts, trampleDamage, controllerDamage };
}

export function DamageAssignmentModal({ data }: { data: AssignCombatDamage["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const { getName, getStats } = useObjectNamer();

  // CR 506.4c: the engine publishes each attacker's live attack target; an
  // attacker whose planeswalker or battle left combat has no entry.
  const attackTarget = useGameStore(
    (s) => s.gameState?.derived?.attack_targets?.[String(data.attacker_id)],
  );
  const hasAttackTarget = attackTarget != null;
  const preset = lethalPreset(data, hasAttackTarget);
  const presetTramples = preset.trampleDamage > 0 || preset.controllerDamage > 0;

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
  // CR 702.19b: ordinary trample's excess goes to whatever the creature is
  // attacking, so the row names the engine-reported planeswalker or battle.
  const trampleLabel = isOverPw
    ? t("combat.planeswalkerLoyalty", { loyalty: data.pw_loyalty ?? 0 })
    : attackTarget && attackTarget.type !== "Player"
      ? t("combat.attackTargetTrample", { name: getName(attackTarget.data) })
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
        <button
          className={gameButtonClass({ tone: "neutral", size: "xs" })}
          // Nothing to do only when the preset is all zeroes and so is the draft.
          disabled={!presetTramples && !preset.amounts.some((amount) => amount > 0) && total === 0}
          onClick={() => {
            setAmounts(preset.amounts);
            setTrampleDamage(preset.trampleDamage);
            setControllerDamage(preset.controllerDamage);
          }}
        >
          {t(presetTramples ? "combat.lethalPresetTrample" : "combat.lethalPreset")}
        </button>
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
              decrementLabel="−"
              incrementLabel="+"
              shortcuts={[
                {
                  key: "lethal",
                  label: t("combat.lethalButton"),
                  ariaLabel: t("combat.assignLethalTo", { target: name }),
                  onClick: () => setAmount(i, Math.min(blocker.lethal_minimum, amounts[i] + remaining)),
                  disabled: isLethal || remaining <= 0,
                },
                ...damageShortcuts(t, name, amounts[i], remaining, (value) => setAmount(i, value)),
              ]}
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
            decrementLabel="−"
            incrementLabel="+"
            shortcuts={damageShortcuts(t, trampleLabel, trampleDamage, remaining, setTrampleDamage)}
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
            decrementLabel="−"
            incrementLabel="+"
            shortcuts={damageShortcuts(t, controllerLabel, controllerDamage, remaining, setControllerDamage)}
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
              decrementLabel="−"
              incrementLabel="+"
              shortcuts={damageShortcuts(t, name, amounts[i], remaining, (value) => setAmount(i, value))}
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
