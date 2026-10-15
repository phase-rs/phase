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
  // CR 115.7d: "choose new targets" (All scope) may leave any position
  // unchanged, so it offers Keep at every position count, one included.
  // "Change the target" (Single scope) has nothing to keep. Slot chips are a
  // separate question: they only appear when there is more than one position.
  const permitsKeep = data.scope.type === "All";
  const isMultiSlot = permitsKeep && slotCount > 1;

  // CR 115.7d: every position starts KEPT (`null`), which leaves its target
  // unchanged with its announced object. A chosen target is sent as itself.
  // "Change the target" (Single scope) has nothing to keep: it needs a pick.
  const [selected, setSelected] = useState<(TargetRef | null)[]>(
    () => Array.from({ length: slotCount }, () => null),
  );
  const [activeSlot, setActiveSlot] = useState(0);

  const handleSelectSingle = useCallback((target: TargetRef) => {
    setSelected([target]);
  }, []);

  const setSlot = useCallback((slotIndex: number, pick: TargetRef | null) => {
    setSelected((prev) => {
      const next = [...prev];
      next[slotIndex] = pick;
      return next;
    });
    if (slotIndex + 1 < slotCount) {
      setActiveSlot(slotIndex + 1);
    }
  }, [slotCount]);

  const handleSelectSlot = useCallback((slotIndex: number, target: TargetRef) => {
    // Choosing the announced object again is the same as keeping it, unless
    // the engine says the choice is a distinct election (its announced
    // object is gone, so the id names a returned object).
    const current = data.current_targets[slotIndex];
    const isKeep = current != null
      && targetsEqual(current, target)
      && !(data.keep_is_distinct?.[slotIndex] ?? false);
    setSlot(slotIndex, isKeep ? null : target);
  }, [data.current_targets, data.keep_is_distinct, setSlot]);

  const handleConfirm = useCallback(() => {
    const payload = permitsKeep
      ? selected.slice(0, slotCount)
      : selected.slice(0, 1);
    dispatch({ type: "RetargetSpell", data: { new_targets: payload } });
  }, [dispatch, permitsKeep, selected, slotCount]);

  const scopeLabel =
    data.scope.type === "Single"
      ? t("retargetChoice.scopeSingle")
      : t("retargetChoice.scopeMulti");

  const currentLabel = data.current_targets
    .map((target) => targetLabel(target, objects))
    .join(", ");

  const confirmDisabled = useMemo(
    () => (permitsKeep ? selected.length < slotCount : selected[0] == null),
    [permitsKeep, selected, slotCount],
  );

  const activeSelection = permitsKeep ? selected[activeSlot] : selected[0];

  // CR 115.7d + INVARIANT SC (phase-rs/phase#8355 round-8 review finding
  // MED-2): admission is PER-SLOT (`engine::apply_retarget`'s `pool_for`),
  // but this modal rendered the FLAT UNION for whichever slot was active —
  // measured on a multi-slot prompt where the union had 5 entries and
  // `slot_pools[0]` had 2, so three rendered choices were rejected on
  // click. Index by the active slot's OWN pool; `slot_pools` empty (an
  // outer-empty compat payload, INVARIANT SC) falls back to the union,
  // which is what a `Legacy`-enforced or pre-field prompt's pool equals
  // anyway.
  const renderSlot = permitsKeep ? activeSlot : 0;
  const slotOptions = data.slot_pools[renderSlot] ?? data.legal_new_targets;

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
      {permitsKeep && (
        <div className="mb-4 flex flex-wrap justify-center gap-2">
          <button
            type="button"
            className={`rounded-md px-3 py-1.5 text-sm font-medium transition ${
              selected[activeSlot] == null
                ? "bg-emerald-600/90 text-white ring-2 ring-emerald-300/70"
                : "bg-slate-800/80 text-slate-200 hover:bg-slate-700/80"
            }`}
            onClick={() => setSlot(activeSlot, null)}
          >
            {t("retargetChoice.keep")}
          </button>
          {isMultiSlot && data.current_targets.map((current, index) => {
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
        {slotOptions.map((target, index) => {
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
                permitsKeep
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
