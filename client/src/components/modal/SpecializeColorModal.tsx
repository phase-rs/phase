import { useCallback, useState } from "react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type { ManaColor, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";

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
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-3xl"
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={selected === null}
          label={t("specializeColor.confirm")}
        />
      }
    >
      <div className="mx-auto mb-6 flex w-fit max-w-3xl flex-wrap items-center justify-center gap-3 sm:mb-10">
        {data.options.map((color, index) => {
          const isSelected = selected === color;
          return (
            <motion.button
              key={color}
              type="button"
              className={`min-h-11 rounded-lg border-2 px-4 py-3 text-sm font-semibold transition sm:px-5 sm:text-base ${
                isSelected
                  ? "border-emerald-400 bg-emerald-500/30 text-white"
                  : "border-gray-600 bg-gray-800/80 text-gray-300 hover:border-gray-400 hover:text-white"
              }`}
              initial={{ opacity: 0, y: 20, scale: 0.95 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.05 + index * 0.03, duration: 0.25 }}
              whileHover={{ scale: 1.05 }}
              onClick={() => setSelected(isSelected ? null : color)}
            >
              {t(COLOR_LABEL_KEYS[color])}
            </motion.button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
