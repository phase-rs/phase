import { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";

import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { objectImageProps } from "../../services/cardImageLookup.ts";
import type { TargetRef, WaitingFor } from "../../adapter/types.ts";
import { CardImage } from "../card/CardImage.tsx";
import { ChoiceOverlay, ConfirmButton, ScrollableCardStrip } from "./ChoiceOverlay.tsx";
import { targetKey, targetLabel } from "./targetRef.ts";

type RetargetChoice = Extract<WaitingFor, { type: "RetargetChoice" }>;

function targetsEqual(a: TargetRef, b: TargetRef): boolean {
  return targetKey(a) === targetKey(b);
}

export function RetargetChoiceModal({ data }: { data: RetargetChoice["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  const slotCount = Math.max(data.current_targets.length, 1);
  const isMultiSlot = data.scope.type === "All" && slotCount > 1;

  // CR 115.7: Default to keeping the current targets unchanged.
  const [selected, setSelected] = useState<TargetRef[]>(data.current_targets);
  const [activeSlot, setActiveSlot] = useState(0);

  const handleSelectSingle = useCallback((target: TargetRef) => {
    setSelected([target]);
  }, []);

  const handleSelectSlot = useCallback((slotIndex: number, target: TargetRef) => {
    setSelected((prev) => {
      const next = [...prev];
      while (next.length < slotCount) {
        next.push(data.current_targets[next.length] ?? target);
      }
      next[slotIndex] = target;
      return next;
    });
    if (slotIndex + 1 < slotCount) {
      setActiveSlot(slotIndex + 1);
    }
  }, [data.current_targets, slotCount]);

  const handleConfirm = useCallback(() => {
    const payload = isMultiSlot
      ? selected.slice(0, slotCount)
      : selected.slice(0, 1);
    dispatch({ type: "RetargetSpell", data: { new_targets: payload } });
  }, [dispatch, isMultiSlot, selected, slotCount]);

  const scopeLabel =
    data.scope.type === "Single"
      ? t("retargetChoice.scopeSingle")
      : t("retargetChoice.scopeMulti");

  const currentLabel = data.current_targets
    .map((target) => targetLabel(target, objects))
    .join(", ");

  const confirmDisabled = useMemo(() => {
    if (selected.length === 0) return true;
    if (!isMultiSlot) return false;
    return selected.length < slotCount
      || selected.slice(0, slotCount).some((target) => target == null);
  }, [isMultiSlot, selected, slotCount]);

  const activeSelection = isMultiSlot ? selected[activeSlot] : selected[0];

  return (
    <ChoiceOverlay
      title={t("retargetChoice.title")}
      subtitle={t("retargetChoice.subtitle", { scope: scopeLabel, current: currentLabel })}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={confirmDisabled}
          label={t("retargetChoice.confirm")}
        />
      }
    >
      {isMultiSlot && (
        <div className="mb-4 flex flex-wrap justify-center gap-2">
          {data.current_targets.map((current, index) => {
            const chosen = selected[index];
            const isActive = index === activeSlot;
            return (
              <button
                key={`slot-${index}`}
                type="button"
                className={`rounded-md px-3 py-1.5 text-sm font-medium transition ${
                  isActive
                    ? "bg-sky-600/90 text-white ring-2 ring-sky-300/70"
                    : "bg-slate-800/80 text-slate-200 hover:bg-slate-700/80"
                }`}
                onClick={() => setActiveSlot(index)}
              >
                {t("retargetChoice.slotLabel", {
                  index: index + 1,
                  current: targetLabel(current, objects),
                  chosen: chosen ? targetLabel(chosen, objects) : t("retargetChoice.unselected"),
                })}
              </button>
            );
          })}
        </div>
      )}
      <ScrollableCardStrip>
        {data.legal_new_targets.map((target, index) => {
          const key = targetKey(target);
          const isSelected = activeSelection != null && targetsEqual(activeSelection, target);
          const obj = "Object" in target ? objects?.[String(target.Object)] : undefined;

          return (
            <motion.button
              key={key}
              className={`relative shrink-0 rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-sky-300/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => (
                isMultiSlot
                  ? handleSelectSlot(activeSlot, target)
                  : handleSelectSingle(target)
              )}
              {...("Object" in target ? hoverProps(target.Object) : {})}
            >
              {obj ? (
                <CardImage {...objectImageProps(obj)} size="normal" />
              ) : (
                <div className="flex h-44 w-32 items-center justify-center rounded-lg border border-white/15 bg-slate-800/80 px-3 text-center text-sm font-semibold text-slate-100">
                  {targetLabel(target, objects)}
                </div>
              )}
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-sky-500/20">
                  <span className="rounded-full bg-sky-500/90 px-3 py-1 text-xs font-bold text-white">
                    {t("retargetChoice.badgeNewTarget")}
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}
