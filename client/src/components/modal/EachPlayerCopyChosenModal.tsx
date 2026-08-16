import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { TargetRef, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { targetKey, targetLabel } from "./targetRef.ts";

type EachPlayerCopyChosenSelection = Extract<
  WaitingFor,
  { type: "EachPlayerCopyChosenSelection" }
>;

// CR 101.4 + CR 707.2 + CR 122.1: EachPlayerCopyChosen — the choosing player
// picks an ordered 1..=max objects they control. Order is load-bearing: the
// first pick is copied; a second pick (only when `max >= 2`) scales the copy.
// Pure display: the engine owns eligibility (`data.eligible`) and all rules;
// this modal only assembles the ordered selection and dispatches `SelectTargets`.
export function EachPlayerCopyChosenModal({
  data,
}: {
  data: EachPlayerCopyChosenSelection["data"];
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  const [selected, setSelected] = useState<TargetRef[]>([]);
  const eligibleKey = data.eligible.map(targetKey).join("|");

  // Reset when a fresh prompt arrives (back-to-back per-player prompts from one
  // resolution don't remount this component).
  useEffect(() => {
    setSelected([]);
  }, [eligibleKey]);

  const handleToggle = useCallback(
    (target: TargetRef) => {
      const key = targetKey(target);
      setSelected((prev) => {
        if (prev.some((tRef) => targetKey(tRef) === key)) {
          return prev.filter((tRef) => targetKey(tRef) !== key);
        }
        if (prev.length >= data.max) {
          return prev;
        }
        return [...prev, target];
      });
    },
    [data.max],
  );

  const handleConfirm = useCallback(() => {
    dispatch({ type: "SelectTargets", data: { targets: selected } });
  }, [dispatch, selected]);

  const canConfirm =
    selected.length >= data.min && selected.length <= data.max;

  return (
    <ChoiceOverlay
      title={t("cardChoice.eachPlayerCopyChosen.title")}
      subtitle={t("cardChoice.eachPlayerCopyChosen.prompt")}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={!canConfirm}
          label={t("cardChoice.eachPlayerCopyChosen.confirm")}
        />
      }
    >
      <div className="mb-4 space-y-2">
        {data.eligible.map((target) => {
          const key = targetKey(target);
          const order = selected.findIndex((tRef) => targetKey(tRef) === key);
          const isSelected = order >= 0;
          // Role label for the ordered pick: first = copied, second = scaler.
          const roleKey =
            order === 0
              ? "cardChoice.eachPlayerCopyChosen.copied"
              : "cardChoice.eachPlayerCopyChosen.scaler";
          return (
            <button
              key={key}
              type="button"
              aria-pressed={isSelected}
              {...("Object" in target ? hoverProps(target.Object) : undefined)}
              onClick={() => handleToggle(target)}
              className={
                gameButtonClass({
                  tone: isSelected ? "blue" : "neutral",
                  size: "md",
                }) + " flex w-full items-center justify-between text-left"
              }
            >
              <span>{targetLabel(target, objects)}</span>
              {isSelected && (
                <span className="ml-2 text-xs opacity-80">
                  {order + 1}
                  {data.max >= 2 ? ` · ${t(roleKey)}` : ""}
                </span>
              )}
            </button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
