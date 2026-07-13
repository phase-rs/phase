import { useCallback, useState } from "react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type { ManaColor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";

// CR 105.3: the five colors, in WUBRG order (matches engine `ManaColor::ALL`).
const COLORS: ManaColor[] = ["White", "Blue", "Black", "Red", "Green"];

// Reuse the generic color-name labels already defined for the specialize picker.
const COLOR_LABEL_KEYS: Record<ManaColor, string> = {
  White: "specializeColor.white",
  Blue: "specializeColor.blue",
  Black: "specializeColor.black",
  Red: "specializeColor.red",
  Green: "specializeColor.green",
};

/**
 * CR 103.2c + CR 903.4b: the pre-game "choose a color before the game begins"
 * prompt for a commander with a linked color CDA (Clara Oswald, The Prismatic
 * Piper, Faceless One). Purely a display layer over the engine-provided
 * `PregameChooseColor` waiting state — it dispatches the chosen color and never
 * computes game state. `commanderName` is engine-provided (the object's name).
 */
export function PregameChooseColorModal({
  commanderName,
}: {
  commanderName: string;
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const [selected, setSelected] = useState<ManaColor | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected !== null) {
      dispatch({ type: "ChoosePregameColor", data: { color: selected } });
    }
  }, [dispatch, selected]);

  return (
    <ChoiceOverlay
      title={t("gamePage.pregameColor.title")}
      subtitle={t("gamePage.pregameColor.subtitle", { name: commanderName })}
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-3xl"
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={selected === null}
          label={t("gamePage.pregameColor.confirm")}
        />
      }
    >
      <div className="mx-auto mb-6 flex w-fit max-w-3xl flex-wrap items-center justify-center gap-3 sm:mb-10">
        {COLORS.map((color, index) => {
          const isSelected = selected === color;
          return (
            <motion.button
              key={color}
              type="button"
              aria-label={t("gamePage.pregameColor.chooseAria", {
                color: t(COLOR_LABEL_KEYS[color]),
                name: commanderName,
              })}
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
