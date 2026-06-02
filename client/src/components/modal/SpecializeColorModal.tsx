import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type { ManaColor, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay } from "./ChoiceOverlay.tsx";

type SpecializeColor = Extract<WaitingFor, { type: "SpecializeColor" }>;

const COLOR_LABEL_KEYS: Record<ManaColor, string> = {
  White: "specializeColor.white",
  Blue: "specializeColor.blue",
  Black: "specializeColor.black",
  Red: "specializeColor.red",
  Green: "specializeColor.green",
};

export function SpecializeColorModal({ data }: { data: SpecializeColor["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const [selected, setSelected] = useState<ManaColor | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected !== null) {
      dispatch({ type: "ChooseSpecializeColor", data: { color: selected } });
    }
  }, [dispatch, selected]);

  return (
    <ChoiceOverlay
      title={t("specializeColor.title")}
      subtitle={t("specializeColor.subtitle")}
      confirmLabel={t("specializeColor.confirm")}
      onConfirm={handleConfirm}
      confirmDisabled={selected === null}
    >
      <div className="choice-grid">
        {data.options.map((color) => (
          <button
            key={color}
            type="button"
            className={`choice-button${selected === color ? " selected" : ""}`}
            onClick={() => setSelected(color)}
          >
            {t(COLOR_LABEL_KEYS[color])}
          </button>
        ))}
      </div>
    </ChoiceOverlay>
  );
}
