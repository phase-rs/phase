import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";

import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { TargetRef, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { targetKey, targetLabel } from "./targetRef.ts";

type RetargetChoice = Extract<WaitingFor, { type: "RetargetChoice" }>;

export function RetargetChoiceModal({ data }: { data: RetargetChoice["data"] }) {
  const { t } = useTranslation("game");
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
      ? t("retargetChoice.scopeSingle")
      : t("retargetChoice.scopeMulti");

  const currentLabel = data.current_targets
    .map((target) => targetLabel(target, objects))
    .join(", ");

  return (
    <ChoiceOverlay
      title={t("retargetChoice.title")}
      subtitle={t("retargetChoice.subtitle", { scope: scopeLabel, current: currentLabel })}
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected.length === 0} label={t("retargetChoice.confirm")} />}
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

