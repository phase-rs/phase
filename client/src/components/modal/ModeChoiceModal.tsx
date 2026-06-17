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

  const toggleMode = useCallback(
    (index: number) => {
      if (unavailableModes.includes(index)) return;
      setSelected((prev) => {
        if (!modal) return prev;

        if (modal.allow_repeat_modes) {
          if (prev.length >= modal.max_choices) {
            return prev;
          }
          return [...prev, index].sort((a, b) => a - b);
        }

        if (prev.includes(index)) {
          return prev.filter((value) => value !== index);
        }
        if (prev.length >= modal.max_choices) {
          return prev;
        }
        return [...prev, index].sort((a, b) => a - b);
      });
    },
    [modal, unavailableModes],
  );

  const handleConfirm = useCallback(() => {
    if (!modal) return;
    const indices = [...selected].sort((a, b) => a - b);
    if (indices.length < modal.min_choices || indices.length > modal.max_choices) return;
    dispatch({ type: "SelectModes", data: { indices } });
    setSelected([]);
  }, [modal, selected, dispatch]);

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
    setSelected([]);
  }, [dispatch]);

  if (!isModeChoice || !isMyChoice || !modal) return null;

  const canConfirm = selected.length >= modal.min_choices && selected.length <= modal.max_choices;
  const isSingleChoice = modal.min_choices === 1 && modal.max_choices === 1;

  const chooseLabel =
    modal.min_choices === modal.max_choices
      ? t("modeChoice.chooseExact", { count: modal.min_choices })
      : t("modeChoice.chooseRange", { min: modal.min_choices, max: modal.max_choices });

  const showFooter = !isSingleChoice || !isAbilityMode;
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
          {t("modeChoice.confirm", { selected: selected.length, count: modal.max_choices })}
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
      {!isAbilityMode && (
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
            const isUnavailable = unavailableModes.includes(index);
            return (
              <button
                key={index}
                disabled={isUnavailable}
                onClick={() => {
                  if (isUnavailable) return;
                  if (isSingleChoice) {
                    dispatch({ type: "SelectModes", data: { indices: [index] } });
                    setSelected([]);
                  } else {
                    toggleMode(index);
                  }
                }}
                className={`rounded-[16px] border px-4 py-3 text-left transition ${
                  isUnavailable
                    ? "cursor-not-allowed border-white/5 bg-white/3 opacity-40"
                    : isSelected
                      ? "border-cyan-300/60 bg-cyan-500/12 ring-1 ring-cyan-400/40"
                      : "border-white/8 bg-white/5 hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
                }`}
              >
                <span className={`font-semibold ${isUnavailable ? "text-slate-500" : "text-white"}`}>{desc}</span>
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
