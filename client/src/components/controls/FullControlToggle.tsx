import { useId } from "react";
import { useTranslation } from "react-i18next";

import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";

export function FullControlToggle({
  className,
  iconOnly = false,
}: {
  className?: string;
  iconOnly?: boolean;
} = {}) {
  const { t } = useTranslation("game");
  const tooltipId = useId();
  const fullControl = useUiStore((s) => s.fullControl);
  const toggleFullControl = useUiStore((s) => s.toggleFullControl);
  const isCompactHeight = useIsCompactHeight();

  // On landscape phones, only show when ON (so the user can turn it off);
  // hide entirely when off so it doesn't eat horizontal space.
  if (isCompactHeight && !fullControl && !iconOnly) return null;

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
      data-full-control-toggle=""
      data-icon-only={iconOnly ? "true" : undefined}
      className={`${iconOnly ? "tabletop-liquid-glass-control h-11 w-11 p-0" : "tabletop-control-chip min-h-11 border px-3 py-1 lg:min-h-0 lg:px-3.5 lg:py-1.5"} group relative flex items-center justify-center text-[10px] font-semibold uppercase tracking-[0.14em] transition-colors duration-150 lg:text-[11px] ${
        fullControl
          ? iconOnly
            ? "border-amber-200/50 text-amber-100 drop-shadow-[0_0_5px_rgba(253,230,138,0.7)]"
            : "border-amber-300/35 bg-amber-950/72 text-amber-100"
          : iconOnly
            ? "border-white/15 text-slate-300 hover:border-white/30 hover:text-white"
            : "border-white/10 bg-slate-950/82 text-slate-300 hover:border-white/20 hover:bg-slate-900 hover:text-white"
      } ${className ?? ""}`}
    >
      {iconOnly ? (
        <svg
          aria-hidden
          className="h-5 w-5"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          strokeWidth="1.7"
        >
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            d="M8 3H3v5m13-5h5v5M8 21H3v-5m13 5h5v-5M12 8.25v7.5m-3.75-3.75h7.5"
          />
        </svg>
      ) : t("fullControl.label")}
      <GameplayTooltip id={tooltipId}>
        {t("fullControl.tooltip")}
      </GameplayTooltip>
    </button>
  );
}
