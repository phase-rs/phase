import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import type { ObjectId, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { gameButtonClass } from "../ui/buttonStyles.ts";

type KeepWithinTotalPowerChoice = Extract<
  WaitingFor,
  { type: "KeepWithinTotalPowerChoice" }
>;

// CR 107.1c + CR 701.21a (Slaughter the Strong): keep any number of eligible
// creatures whose combined power is at most `cap`; the rest are sacrificed. The
// engine pre-filters `eligible` by controller and the effect's creature filter,
// so this modal is purely the subset chooser. Confirm is disabled while the
// running total exceeds the cap.
export function KeepWithinTotalPowerChoiceModal({
  data,
}: {
  data: KeepWithinTotalPowerChoice["data"];
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  const [kept, setKept] = useState<ObjectId[]>([]);

  // Reset the selection when a fresh prompt arrives — back-to-back per-player
  // states from one resolution don't remount this component.
  useEffect(() => {
    setKept([]);
  }, [data.eligible]);

  const toggle = useCallback((id: ObjectId) => {
    setKept((prev) =>
      prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id],
    );
  }, []);

  const handleConfirm = useCallback(() => {
    dispatch({ type: "ChooseKeptCreatures", data: { kept } });
  }, [dispatch, kept]);

  if (!objects) return null;

  const powerOf = (id: ObjectId) => objects[id]?.power ?? 0;
  const total = kept.reduce((sum, id) => sum + powerOf(id), 0);
  const withinCap = total <= data.cap;

  return (
    <ChoiceOverlay
      title={t("keepWithinTotalPower.title")}
      subtitle={t("keepWithinTotalPower.subtitle", { cap: data.cap })}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!withinCap} />}
    >
      <div
        className={
          "mb-2 text-sm font-bold uppercase tracking-wide " +
          (withinCap ? "text-slate-300" : "text-red-400")
        }
      >
        {total} / {data.cap}
      </div>
      <div className="mb-4 space-y-2">
        {data.eligible.map((id) => {
          const isSelected = kept.includes(id);
          return (
            <button
              key={id}
              type="button"
              aria-pressed={isSelected}
              onClick={() => toggle(id)}
              className={
                gameButtonClass({
                  tone: isSelected ? "blue" : "neutral",
                  size: "md",
                }) + " w-full text-left"
              }
              {...hoverProps(id)}
            >
              {objects[id]?.name ?? t("categoryChoice.objectFallback", { id })} (
              {powerOf(id)})
            </button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
