import { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type { ModalChoice } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

export function ModeChoiceModal() {
  const { t } = useTranslation("game");
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const [selected, setSelected] = useState<number[]>([]);

  const isModeChoice = waitingFor?.type === "ModeChoice" || waitingFor?.type === "AbilityModeChoice";
  const isAbilityMode = waitingFor?.type === "AbilityModeChoice";
  const modal: ModalChoice | null = isModeChoice ? waitingFor.data.modal : null;
  const sourceObjectId = !isModeChoice
    ? undefined
    : waitingFor.type === "AbilityModeChoice"
      ? waitingFor.data.source_id
      : waitingFor.data.pending_cast.object_id;
  const unavailableModes: number[] = useMemo(
    () =>
      isModeChoice && "unavailable_modes" in waitingFor.data
        ? (waitingFor.data.unavailable_modes ?? [])
        : [],
    [isModeChoice, waitingFor],
  );
  const isMyChoice = isModeChoice && canActForWaitingState;

  // CR 700.2i: pawprint points-budget selection math. When `mode_pawprints` is
  // non-empty the modal is a budget modal: `max_choices` is the point budget
  // (Σ of chosen mode weights ≤ budget), NOT a mode count. The engine remains
  // the legality authority; this is pure display arithmetic over engine data.
  const pawprints = useMemo(() => modal?.mode_pawprints ?? [], [modal]);
  const isBudget = pawprints.length > 0;
  const budget = modal?.max_choices ?? 0;
  const spent = useMemo(
    () => (isBudget ? selected.reduce((sum, i) => sum + (pawprints[i] ?? 0), 0) : 0),
    [isBudget, selected, pawprints],
  );

  const toggleMode = useCallback(
    (index: number) => {
      if (unavailableModes.includes(index)) return;
      setSelected((prev) => {
        if (!modal) return prev;

        if (modal.allow_repeat_modes) {
          if (isBudget) {
            const spentPrev = prev.reduce((sum, i) => sum + (pawprints[i] ?? 0), 0);
            if (spentPrev + (pawprints[index] ?? 0) > budget) {
              return prev;
            }
          } else if (prev.length >= modal.max_choices) {
            return prev;
          }
          return [...prev, index].sort((a, b) => a - b);
        }

        if (prev.includes(index)) {
          return prev.filter((value) => value !== index);
        }
        if (isBudget) {
          const spentPrev = prev.reduce((sum, i) => sum + (pawprints[i] ?? 0), 0);
          if (spentPrev + (pawprints[index] ?? 0) > budget) {
            return prev;
          }
        } else if (prev.length >= modal.max_choices) {
          return prev;
        }
        return [...prev, index].sort((a, b) => a - b);
      });
    },
    [modal, unavailableModes, isBudget, pawprints, budget],
  );

  const handleConfirm = useCallback(() => {
    if (!modal) return;
    const indices = [...selected].sort((a, b) => a - b);
    if (indices.length < modal.min_choices) return;
    if (isBudget) {
      const spentSum = indices.reduce((sum, i) => sum + (pawprints[i] ?? 0), 0);
      if (spentSum > modal.max_choices) return;
    } else if (indices.length > modal.max_choices) {
      return;
    }
    dispatch({ type: "SelectModes", data: { indices } });
    setSelected([]);
  }, [modal, selected, dispatch, isBudget, pawprints]);

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
    setSelected([]);
  }, [dispatch]);

  if (!isModeChoice || !isMyChoice || !modal) return null;

  const canConfirm =
    selected.length >= modal.min_choices &&
    (isBudget ? spent <= budget : selected.length <= modal.max_choices);
  // A pawprint budget modal (budget 5, min weight 1) is never a single-choice
  // dispatch; only a genuine 1-of-1 count modal collapses to immediate dispatch.
  const isSingleChoice = !isBudget && modal.min_choices === 1 && modal.max_choices === 1;
  const canAdd = (index: number) => !isBudget || spent + (pawprints[index] ?? 0) <= budget;

  // CR 602.2b vs CR 603.3c: only an ACTIVATED modal ability is cancellable at the
  // mode-choice step; a triggered one must resolve its mode. Read engine-provided
  // is_activated — no game-state inference here.
  const isActivatedAbilityMode =
    waitingFor?.type === "AbilityModeChoice" && waitingFor.data.is_activated === true;
  const canCancel = !isAbilityMode || isActivatedAbilityMode;

  const chooseLabel =
    modal.min_choices === modal.max_choices
      ? t("modeChoice.chooseExact", { count: modal.min_choices })
      : t("modeChoice.chooseRange", { min: modal.min_choices, max: modal.max_choices });

  const showFooter = !isSingleChoice || canCancel;
  const footer = showFooter ? (
    <div className="flex flex-col gap-3 sm:flex-row sm:justify-end">
      {!isSingleChoice && (
        <button
          onClick={handleConfirm}
          disabled={!canConfirm}
          className={`min-h-11 rounded-[16px] px-6 py-2 font-semibold transition ${
            canConfirm
              ? "bg-cyan-500 text-slate-950 shadow-[0_14px_34px_rgba(6,182,212,0.28)] hover:bg-cyan-400"
              : "cursor-not-allowed border border-white/8 bg-white/5 text-slate-500"
          }`}
        >
          {isBudget
            ? t("modeChoice.confirmBudget", { spent, budget })
            : t("modeChoice.confirm", { selected: selected.length, count: modal.max_choices })}
        </button>
      )}
      {!isSingleChoice && selected.length > 0 && (
        <button
          onClick={() => setSelected([])}
          className="min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-6 py-2 font-semibold text-slate-200 transition hover:bg-white/8"
        >
          {t("modeChoice.clear")}
        </button>
      )}
      {canCancel && (
        <button
          onClick={handleCancel}
          className="min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-6 py-2 font-semibold text-slate-200 transition hover:bg-white/8"
        >
          {t("common:actions.cancel")}
        </button>
      )}
    </div>
  ) : undefined;

  return (
    <DialogShell
      eyebrow={isAbilityMode ? t("modeChoice.eyebrowAbility") : t("modeChoice.eyebrowSpell")}
      title={chooseLabel}
      subtitle={t("modeChoice.subtitle")}
      size="md"
      scrollable
      footer={footer}
      previewObjectId={sourceObjectId}
    >
      <div className="px-3 py-3 lg:px-5 lg:py-5">
        <div className="flex flex-col gap-2">
          {modal.mode_descriptions.map((desc, index) => {
            const count = selected.filter((value) => value === index).length;
            const isSelected = count > 0;
            const weight = pawprints[index] ?? 0;
            // CR 700.2i: a budget mode that no longer fits the remaining budget
            // is greyed like an unavailable mode (unless it's already selected in
            // a non-repeat modal, where the row toggles it back off).
            const budgetBlocked = isBudget && !canAdd(index) && !(isSelected && !modal.allow_repeat_modes);
            const isUnavailable = unavailableModes.includes(index);
            const isDisabled = isUnavailable || budgetBlocked;
            return (
              <button
                key={index}
                disabled={isDisabled}
                onClick={() => {
                  if (isDisabled) return;
                  if (isSingleChoice) {
                    dispatch({ type: "SelectModes", data: { indices: [index] } });
                    setSelected([]);
                  } else {
                    toggleMode(index);
                  }
                }}
                className={`rounded-[16px] border px-4 py-3 text-left transition ${
                  isDisabled
                    ? "cursor-not-allowed border-white/5 bg-white/3 opacity-40"
                    : isSelected
                      ? "border-cyan-300/60 bg-cyan-500/12 ring-1 ring-cyan-400/40"
                      : "border-white/8 bg-white/5 hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
                }`}
              >
                {isBudget && weight > 0 && (
                  <span className="mr-2 inline-flex items-center justify-center rounded-full bg-amber-300/20 px-2 py-0.5 text-xs font-semibold text-amber-100">
                    {"{P}".repeat(weight)}
                  </span>
                )}
                <span className={`font-semibold ${isDisabled ? "text-slate-500" : "text-white"}`}>{desc}</span>
                {isUnavailable && (
                  <span className="ml-2 text-xs text-slate-500">{t("modeChoice.alreadyChosen")}</span>
                )}
                {count > 0 && (
                  <span className="ml-2 inline-flex min-w-6 items-center justify-center rounded-full bg-cyan-300/20 px-2 py-0.5 text-xs font-semibold text-cyan-100">
                    {count}
                  </span>
                )}
              </button>
            );
          })}
        </div>
      </div>
    </DialogShell>
  );
}
