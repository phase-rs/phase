import { useCallback, useEffect, useState } from "react";

import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { TargetRef, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { targetKey, targetLabel } from "./targetRef.ts";

type ProliferateChoice = Extract<WaitingFor, { type: "ProliferateChoice" }>;
type ChooseObjectsSelection = Extract<
  WaitingFor,
  { type: "ChooseObjectsSelection" }
>;

// CR 701.34a: Proliferate — choose any number (including zero) of permanents
// and players that have counters; each chosen target gets one more counter of
// each kind already there.
// CR 603.7e: ChooseObjectsSelection — choose any number of battlefield
// permanents (Magnetic Mountain class). Both prompts carry the identical
// `{ player, eligible: TargetRef[] }` shape and dispatch `SelectTargets`, so a
// single modal serves both; `variant` only adapts the title/subtitle copy.
// Engine pre-filters `eligible`; the modal is purely a chooser. Default-select-
// all is a UX choice (one-click confirm for the common case), not a rules
// requirement.
type ProliferateModalData =
  | ProliferateChoice["data"]
  | ChooseObjectsSelection["data"];

const VARIANT_COPY = {
  proliferate: {
    title: "Proliferate",
    subtitle:
      "Choose any number of permanents and players with counters. Each chosen target gets one more counter of each kind already there.",
  },
  chooseObjects: {
    title: "Choose Permanents",
    subtitle:
      "Choose any number of permanents. You pay a cost for each one chosen.",
  },
} as const;

export function ProliferateModal({
  data,
  variant = "proliferate",
}: {
  data: ProliferateModalData;
  variant?: keyof typeof VARIANT_COPY;
}) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);

  const [selected, setSelected] = useState<TargetRef[]>(data.eligible);

  // Reset selection when a fresh choice arrives (back-to-back prompts from one
  // ability resolution don't remount this component).
  useEffect(() => {
    setSelected(data.eligible);
  }, [data.eligible]);

  const handleToggle = useCallback((target: TargetRef) => {
    const key = targetKey(target);
    setSelected((prev) =>
      prev.some((t) => targetKey(t) === key)
        ? prev.filter((t) => targetKey(t) !== key)
        : [...prev, target],
    );
  }, []);

  const handleConfirm = useCallback(() => {
    dispatch({ type: "SelectTargets", data: { targets: selected } });
  }, [dispatch, selected]);

  return (
    <ChoiceOverlay
      title={VARIANT_COPY[variant].title}
      subtitle={VARIANT_COPY[variant].subtitle}
      footer={<ConfirmButton onClick={handleConfirm} label="Confirm" />}
    >
      <div className="mb-4 space-y-2">
        {data.eligible.map((target) => {
          const key = targetKey(target);
          const isSelected = selected.some((t) => targetKey(t) === key);
          return (
            <button
              key={key}
              type="button"
              aria-pressed={isSelected}
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
