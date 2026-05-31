import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { getPlayerDisplayName } from "../../stores/multiplayerStore.ts";
import type { ObjectId, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";

type CategoryChoice = Extract<WaitingFor, { type: "CategoryChoice" }>;

// CR 101.4 + CR 701.21a: The engine pre-filters `eligible_per_category` by
// controller, category, and effect-specific filters; this modal is purely the
// chooser. An object that belongs to multiple categories may appear in each
// eligible list and may be submitted for each matching category slot.
//
// CR 800.4g: if `target_player` has left the game mid-resolution, the engine
// substitutes the choice — that leaver path is handled engine-side and is out
// of scope for this display layer.
export function CategoryChoiceModal({ data }: { data: CategoryChoice["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  const [choices, setChoices] = useState<(ObjectId | null)[]>(() =>
    data.categories.map(() => null),
  );

  // Reset selection when a fresh CategoryChoice arrives — back-to-back
  // per-player states from one ability resolution don't remount this component.
  useEffect(() => {
    setChoices(data.eligible_per_category.map(() => null));
  }, [data.eligible_per_category]);

  const handleSelect = useCallback((categoryIndex: number, id: ObjectId) => {
    setChoices((prev) => {
      const next = [...prev];
      next[categoryIndex] = prev[categoryIndex] === id ? null : id;
      return next;
    });
  }, []);

  const handleConfirm = useCallback(() => {
    dispatch({ type: "SelectCategoryPermanents", data: { choices } });
  }, [dispatch, choices]);

  if (!objects) return null;

  const readyToConfirm = data.eligible_per_category.every(
    (eligible, index) => eligible.length === 0 || choices[index] != null,
  );
  const choosingForOpponent = data.player !== data.target_player;
  const subtitle = choosingForOpponent
    ? t("categoryChoice.subtitleOpponent", {
        name: getPlayerDisplayName(data.target_player),
      })
    : t("categoryChoice.subtitleSelf");

  return (
    <ChoiceOverlay
      title={t("categoryChoice.title")}
      subtitle={subtitle}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!readyToConfirm} />}
    >
      <div className="mb-4 space-y-4">
        {data.categories.map((category, categoryIndex) => {
          const eligible = data.eligible_per_category[categoryIndex] ?? [];
          return (
            <div key={`${category}-${categoryIndex}`} className="space-y-2">
              <h3 className="text-sm font-bold uppercase tracking-wide text-slate-300">
                {category}
              </h3>
              {eligible.length === 0 ? (
                <button
                  type="button"
                  disabled
                  className={
                    gameButtonClass({ tone: "neutral", size: "md", disabled: true }) +
                    " w-full text-left"
                  }
                >
                  {t("categoryChoice.noneToKeep", { category })}
                </button>
              ) : (
                eligible.map((id) => {
                  const isSelected = choices[categoryIndex] === id;
                  return (
                    <button
                      key={id}
                      type="button"
                      aria-pressed={isSelected}
                      onClick={() => handleSelect(categoryIndex, id)}
                      className={
                        gameButtonClass({
                          tone: isSelected ? "blue" : "neutral",
                          size: "md",
                        }) + " w-full text-left"
                      }
                      {...hoverProps(id)}
                    >
                      {objects[id]?.name ?? t("categoryChoice.objectFallback", { id })}
                    </button>
                  );
                })
              )}
            </div>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
