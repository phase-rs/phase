import { useCallback, useState } from "react";

import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { TargetRef, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { targetKey, targetLabel } from "./targetRef.ts";

type RetargetChoice = Extract<WaitingFor, { type: "RetargetChoice" }>;
type CopyRetarget = Extract<WaitingFor, { type: "CopyRetarget" }>;
type CopyTargetSlot = CopyRetarget["data"]["target_slots"][number];

function targetOptions(slot: CopyTargetSlot): TargetRef[] {
  const seen = new Set<string>();
  const options = slot.legal_alternatives.length > 0
    ? slot.legal_alternatives
    : [slot.current];
  return options.filter((target) => {
    const key = targetKey(target);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

export function RetargetChoiceModal({ data }: { data: RetargetChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);

  // CR 115.7: Default to keeping the current targets unchanged.
  const [selected, setSelected] = useState<TargetRef[]>(data.current_targets);

  const handleSelect = useCallback((target: TargetRef) => {
    setSelected([target]);
  }, []);

  const handleConfirm = useCallback(() => {
    dispatch({ type: "RetargetSpell", data: { new_targets: selected } });
  }, [dispatch, selected]);

  const scopeLabel =
    data.scope.type === "Single"
      ? "Choose a new target for the spell"
      : "Choose new targets for the spell";

  const currentLabel = data.current_targets
    .map((t) => targetLabel(t, objects))
    .join(", ");

  return (
    <ChoiceOverlay
      title="Change Target"
      subtitle={`${scopeLabel}. Current: ${currentLabel}`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected.length === 0} label="Confirm" />}
    >
      <div className="mb-4 space-y-2">
        {data.legal_new_targets.map((target) => {
          const key = targetKey(target);
          const isSelected = selected.some(
            (s) => targetKey(s) === key,
          );
          return (
            <button
              key={key}
              onClick={() => handleSelect(target)}
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

export function CopyRetargetModal({ data }: { data: CopyRetarget["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const [selected, setSelected] = useState<TargetRef[]>(
    () => data.target_slots.map((slot) => targetOptions(slot)[0] ?? slot.current),
  );

  const handleSelect = useCallback((slotIndex: number, target: TargetRef) => {
    setSelected((current) =>
      current.map((value, index) => (index === slotIndex ? target : value)),
    );
  }, []);

  const handleConfirm = useCallback(() => {
    dispatch({ type: "SelectTargets", data: { targets: selected } });
  }, [dispatch, selected]);

  return (
    <ChoiceOverlay
      title="Choose Copy Targets"
      subtitle="Keep the current targets or choose legal alternatives for each slot"
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected.length !== data.target_slots.length} label="Confirm" />}
    >
      <div className="mb-4 space-y-4">
        {data.target_slots.map((slot, slotIndex) => (
          <div
            key={`${slotIndex}-${targetKey(slot.current)}`}
            className="rounded-lg border border-white/10 bg-slate-950/45 p-3"
          >
            <div className="mb-2 flex flex-col gap-1 sm:flex-row sm:items-baseline sm:justify-between">
              <h3 className="text-sm font-semibold text-slate-100">
                Target {slotIndex + 1}
              </h3>
              <p className="text-xs text-slate-400">
                Current: {targetLabel(slot.current, objects)}
              </p>
            </div>
            <div className="space-y-2">
              {targetOptions(slot).map((target) => {
                const key = targetKey(target);
                const selectedTarget = selected[slotIndex];
                const isSelected = selectedTarget ? targetKey(selectedTarget) === key : false;
                const isCurrent = targetKey(slot.current) === key;
                return (
                  <button
                    key={key}
                    type="button"
                    onClick={() => handleSelect(slotIndex, target)}
                    className={
                      gameButtonClass({
                        tone: isSelected ? "blue" : "neutral",
                        size: "md",
                      }) + " w-full justify-between text-left"
                    }
                  >
                    <span>{targetLabel(target, objects)}</span>
                    {isCurrent && (
                      <span className="ml-3 text-xs font-semibold uppercase tracking-wide opacity-75">
                        Current
                      </span>
                    )}
                  </button>
                );
              })}
            </div>
          </div>
        ))}
      </div>
    </ChoiceOverlay>
  );
}
