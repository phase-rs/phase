import { useCallback, useEffect, useState } from "react";

import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { TargetRef, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { targetKey, targetLabel } from "./targetRef.ts";

type ProliferateChoice = Extract<WaitingFor, { type: "ProliferateChoice" }>;

// CR 701.34a: Proliferate — choose any number (including zero) of permanents
// and players that have counters; each chosen target gets one more counter of
// each kind already there. Engine pre-filters `eligible`; the modal is purely
// a chooser. Default-select-all is a UX choice (one-click confirm for the
// common case), not a rules requirement.
export function ProliferateModal({ data }: { data: ProliferateChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);

  const [selected, setSelected] = useState<TargetRef[]>(data.eligible);

  // Reset selection when a fresh ProliferateChoice arrives (back-to-back
  // proliferates from one ability resolution don't remount this component).
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
      title="Proliferate"
      subtitle="Choose any number of permanents and players with counters. Each chosen target gets one more counter of each kind already there."
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
