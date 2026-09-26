import { useTranslation } from "react-i18next";

import { useUiStore } from "../../stores/uiStore.ts";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";

/**
 * Thin pill that forces Manual mana payment for the current game only. Toggles
 * the ephemeral `manualManaOverride` in uiStore (reset on every game boundary by
 * `clearPromptOverlayState`) — it never touches the persisted `spellPaymentMode`
 * preference. Pure display + dispatch leaf; the local player HUD chooses its
 * responsive placement.
 */
export function ManualManaToggle({ iconOnly = false }: { iconOnly?: boolean }) {
  const { t } = useTranslation("game");
  const manualManaOverride = useUiStore((s) => s.manualManaOverride);
  const toggleManualMana = useUiStore((s) => s.toggleManualManaOverride);
  const stateClass = iconOnly
    ? manualManaOverride
      ? "text-cyan-200 drop-shadow-[0_0_5px_rgba(103,232,249,0.8)]"
      : "text-gray-300/80 hover:text-white"
    : manualManaOverride
      ? "bg-cyan-500/20 text-cyan-200 ring-1 ring-cyan-400/40"
      : "bg-gray-800/80 text-gray-400 hover:bg-gray-700/80 hover:text-gray-200";

  return (
    <button
      type="button"
      onClick={toggleManualMana}
      aria-pressed={manualManaOverride}
      aria-label={t("mana.manualMana")}
      data-icon-only={iconOnly ? "true" : undefined}
      data-manual-mana-toggle=""
      className={`group relative inline-flex items-center justify-center gap-1 text-[10px] font-medium transition-colors before:absolute before:-inset-2 before:content-[''] ${iconOnly ? "tabletop-liquid-glass-control h-11 w-11 p-0 focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-200/70" : "rounded-full px-2 py-0.5"} ${stateClass}`}
    >
      {iconOnly ? (
        <svg
          aria-hidden
          className="h-[18px] w-[18px]"
          fill="none"
          viewBox="0 0 20 20"
          stroke="currentColor"
          strokeWidth="1.6"
        >
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            d="M10 2.5 13.25 7 10 11.5 6.75 7 10 2.5Zm-5.75 8.75L7 15l-2.75 2.5L1.5 15l2.75-3.75Zm11.5 0L18.5 15l-2.75 2.5L13 15l2.75-3.75Z"
          />
        </svg>
      ) : (
        <span data-control-label="">{t("mana.manualMana")}</span>
      )}
      <GameplayTooltip>{t("mana.manualManaTooltip")}</GameplayTooltip>
    </button>
  );
}
