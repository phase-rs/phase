import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { TargetRef, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { filterTargetsByController, targetKey, targetLabel } from "./targetRef.ts";

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

/** Maps the variant prop to its i18n leaf pair under `proliferate.*`. */
const VARIANT_KEYS = {
  proliferate: { title: "proliferateTitle", subtitle: "proliferateSubtitle" },
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
      title={t(`proliferate.${VARIANT_KEYS[variant].title}`)}
      subtitle={t(`proliferate.${VARIANT_KEYS[variant].subtitle}`)}
      footer={<ConfirmButton onClick={handleConfirm} label={t("proliferate.confirm")} />}
    >
      {data.eligible.length > 1 && (
        <div className="mb-3 flex flex-wrap gap-2">
          <button
            type="button"
            onClick={() => setSelected(data.eligible)}
            className={gameButtonClass({ tone: "neutral", size: "xs" })}
          >
            {t("proliferate.selectAll")}
          </button>
          <button
            type="button"
            onClick={() => setSelected([])}
            className={gameButtonClass({ tone: "neutral", size: "xs" })}
          >
            {t("proliferate.selectNone")}
          </button>
          <button
            type="button"
            onClick={() => setSelected(filterTargetsByController(data.eligible, objects, playerId))}
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
