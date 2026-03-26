import { useCallback, useState } from "react";

import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { GameObject, TargetRef, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";

type RetargetChoice = Extract<WaitingFor, { type: "RetargetChoice" }>;

function targetLabel(target: TargetRef, objects: Record<string, GameObject> | undefined): string {
  if ("Object" in target) {
    return objects?.[String(target.Object)]?.name ?? `Object ${target.Object}`;
  }
  return `Player ${target.Player}`;
}

function targetKey(target: TargetRef): string {
  if ("Object" in target) return `obj-${target.Object}`;
  return `player-${target.Player}`;
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
