import { useTranslation } from "react-i18next";

import { gameButtonClass } from "../ui/buttonStyles.ts";

interface AttackerControlsProps {
  onAttackAll: () => void;
  onSkip: () => void;
  onConfirm: () => void;
  attackerCount: number;
  /** CR 702.22c: toggle the current attacker selection into a band. */
  onToggleBand: () => void;
  isBanded: boolean;
  /** True when the current selection forms a legal band (offers the toggle). */
  canBand: boolean;
}

export function AttackerControls({
  onAttackAll,
  onSkip,
  onConfirm,
  attackerCount,
  onToggleBand,
  isBanded,
  canBand,
}: AttackerControlsProps) {
  const { t } = useTranslation("game");
  return (
    <div className="fixed inset-x-0 bottom-24 z-30 flex justify-center px-3">
      <div className="flex w-full max-w-[min(26rem,calc(100vw-1.25rem))] flex-col gap-2 rounded-[20px] border border-white/10 bg-[#0b1020]/88 p-2 shadow-[0_20px_48px_rgba(0,0,0,0.44)] backdrop-blur-md sm:w-auto sm:max-w-none sm:flex-row">
      <button
        onClick={onAttackAll}
        className={gameButtonClass({ tone: "amber", size: "md", className: "w-full sm:w-auto" })}
      >
        {t("combat.attackAll")}
      </button>
      {(canBand || isBanded) && (
        <button
          onClick={onToggleBand}
          className={gameButtonClass({ tone: "indigo", size: "md", className: "w-full sm:w-auto sm:min-w-[10.5rem]" })}
        >
          {isBanded ? t("combat.banded") : t("combat.band")}
        </button>
      )}
      <button
        onClick={onSkip}
        className={gameButtonClass({ tone: "slate", size: "md", className: "w-full sm:w-auto sm:min-w-[10.5rem]" })}
      >
        {t("combat.skip")}
      </button>
      <button
        onClick={onConfirm}
        className={gameButtonClass({ tone: "emerald", size: "md", className: "w-full sm:w-auto sm:min-w-[10.5rem]" })}
      >
        {t("combat.confirmAttackers", { count: attackerCount })}
      </button>
      </div>
    </div>
  );
}
