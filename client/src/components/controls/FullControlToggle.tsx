import { useId } from "react";
import { useTranslation } from "react-i18next";

import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";

export function FullControlToggle({ className }: { className?: string } = {}) {
  const { t } = useTranslation("game");
  const tooltipId = useId();
  const fullControl = useUiStore((s) => s.fullControl);
  const toggleFullControl = useUiStore((s) => s.toggleFullControl);
  const isCompactHeight = useIsCompactHeight();

  // On landscape phones, only show when ON (so the user can turn it off);
  // hide entirely when off so it doesn't eat horizontal space.
  if (isCompactHeight && !fullControl) return null;

  return (
    <button
      onClick={toggleFullControl}
      aria-describedby={tooltipId}
      // Toggle semantics + descriptive state live in ARIA so the visible label
      // can stay a compact "Control" (the long "Full Control On/Off" text
      // dominated the action row); on-state is conveyed by the amber styling and
      // the tooltip elaborates.
      aria-pressed={fullControl}
      aria-label={fullControl ? t("fullControl.on") : t("fullControl.off")}
      className={`group relative flex items-center justify-center rounded-full border px-3 py-1 text-[10px] font-semibold uppercase tracking-[0.18em] backdrop-blur-xl transition-all duration-200 lg:px-3.5 lg:py-1.5 lg:text-[11px] ${
        fullControl
          ? "border-amber-300/35 bg-amber-500/18 text-amber-100 shadow-[0_10px_24px_rgba(245,158,11,0.2)]"
          : "border-white/10 bg-slate-950/64 text-slate-300 hover:border-white/20 hover:text-white"
      } ${className ?? ""}`}
    >
      {t("fullControl.label")}
      <GameplayTooltip id={tooltipId}>
        {t("fullControl.tooltip")}
      </GameplayTooltip>
    </button>
  );
}
