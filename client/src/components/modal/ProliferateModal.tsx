import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { TargetRef, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { filterTargetsByController, targetKey, targetLabel } from "./targetRef.ts";

type ProliferateChoice = Extract<WaitingFor, { type: "ProliferateChoice" }>;
type TimeTravelChoice = Extract<WaitingFor, { type: "TimeTravelChoice" }>;
type ChooseObjectsSelection = Extract<
  WaitingFor,
  { type: "ChooseObjectsSelection" }
>;

// CR 701.34a: Proliferate — choose any number (including zero) of permanents
// and players that have counters; each chosen target gets one more counter of
// each kind already there.
// CR 701.56a: Time travel — choose any number of eligible objects for the
// current remove/add phase; each selected object gets exactly that phase's
// counter operation.
// CR 608.2c: ChooseObjectsSelection — choose within the engine-published
// min/max range of battlefield permanents. These prompts share the same
// `SelectTargets` dispatch over an engine-provided `eligible` list, so a single
// modal serves them; `variant` adapts copy, bounds, and default selection.
// Engine pre-filters `eligible`; the modal is purely a chooser.
type ProliferateModalData =
  | ProliferateChoice["data"]
  | TimeTravelChoice["data"]
  | ChooseObjectsSelection["data"];

/** Maps the variant prop to its i18n leaf pair under `proliferate.*`. */
const VARIANT_KEYS = {
  proliferate: { title: "proliferateTitle", subtitle: "proliferateSubtitle" },
  timeTravelRemove: { title: "timeTravelTitle", subtitle: "timeTravelRemoveSubtitle" },
  timeTravelAdd: { title: "timeTravelTitle", subtitle: "timeTravelAddSubtitle" },
  chooseObjects: { title: "chooseObjectsTitle", subtitle: "chooseObjectsSubtitle" },
} as const;

export function ProliferateModal({
  data,
  variant = "proliferate",
}: {
  data: ProliferateModalData;
  variant?: keyof typeof VARIANT_KEYS;
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const playerId = usePlayerId();
  const hoverProps = useInspectHoverProps();
  const chooseObjectsData =
    variant === "chooseObjects" ? (data as ChooseObjectsSelection["data"]) : undefined;

  const max = Math.max(
    0,
    Math.min(
      data.eligible.length,
      chooseObjectsData?.max ?? data.eligible.length,
    ),
  );
  const min = Math.min(max, chooseObjectsData?.min ?? 0);

  const boundedSelection = useCallback(
    (targets: TargetRef[]) => {
      const selectedKeys = new Set<string>();
      const bounded = targets.filter((target) => {
        const key = targetKey(target);
        if (selectedKeys.has(key)) return false;
        selectedKeys.add(key);
        return true;
      }).slice(0, max);
      for (const target of data.eligible) {
        if (bounded.length >= min) break;
        const key = targetKey(target);
        if (!selectedKeys.has(key)) {
          selectedKeys.add(key);
          bounded.push(target);
        }
      }
      return bounded;
    },
    [data.eligible, max, min],
  );

  const defaultSelection = useCallback(
    () => boundedSelection(variant === "timeTravelAdd" ? [] : data.eligible),
    [boundedSelection, data.eligible, variant],
  );
  const [selected, setSelected] = useState<TargetRef[]>(() => defaultSelection());

  // Reset selection when a fresh choice arrives (back-to-back prompts from one
  // ability resolution don't remount this component).
  useEffect(() => {
    setSelected(defaultSelection());
  }, [defaultSelection]);

  const handleToggle = useCallback(
    (target: TargetRef) => {
      const key = targetKey(target);
      setSelected((prev) => {
        const selectedAlready = prev.some((t) => targetKey(t) === key);
        if (selectedAlready) {
          return prev.filter((t) => targetKey(t) !== key);
        }
        return prev.length < max ? [...prev, target] : prev;
      });
    },
    [max],
  );

  const handleConfirm = useCallback(() => {
    dispatch({ type: "SelectTargets", data: { targets: selected } });
  }, [dispatch, selected]);

  const selectionValid = selected.length >= min && selected.length <= max;

  return (
    <ChoiceOverlay
      title={t(`proliferate.${VARIANT_KEYS[variant].title}`)}
      subtitle={t(`proliferate.${VARIANT_KEYS[variant].subtitle}`)}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={!selectionValid}
          label={t("proliferate.confirm")}
        />
      }
    >
      {data.eligible.length > 1 && (
        <div className="mb-3 flex flex-wrap gap-2">
          <button
            type="button"
            onClick={() => setSelected(boundedSelection(data.eligible))}
            className={gameButtonClass({ tone: "neutral", size: "xs" })}
          >
            {t("proliferate.selectAll")}
          </button>
          {min === 0 && (
            <button
              type="button"
              onClick={() => setSelected([])}
              className={gameButtonClass({ tone: "neutral", size: "xs" })}
            >
              {t("proliferate.selectNone")}
            </button>
          )}
          <button
            type="button"
            onClick={() =>
              setSelected(
                boundedSelection(filterTargetsByController(data.eligible, objects, playerId)),
              )
            }
            className={gameButtonClass({ tone: "neutral", size: "xs" })}
          >
            {t("proliferate.selectMine")}
          </button>
        </div>
      )}
      <div className="mb-4 space-y-2">
        {data.eligible.map((target) => {
          const key = targetKey(target);
          const isSelected = selected.some((t) => targetKey(t) === key);
          const disabled = !isSelected && selected.length >= max;
          return (
            <button
              key={key}
              type="button"
              aria-pressed={isSelected}
              disabled={disabled}
              {...("Object" in target ? hoverProps(target.Object) : undefined)}
              onClick={() => handleToggle(target)}
              className={
                gameButtonClass({
                  tone: isSelected ? "blue" : "neutral",
                  size: "md",
                }) + " w-full text-left"
              }
            >
              {targetLabel(target, objects)}
            </button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
