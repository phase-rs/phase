import { useTranslation } from "react-i18next";

import { useUiStore } from "../../stores/uiStore.ts";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";

/**
 * Thin pill that forces Manual mana payment for the current game only. Toggles
 * the ephemeral `manualManaOverride` in uiStore (reset on every game boundary by
 * `clearPromptOverlayState`) — it never touches the persisted `spellPaymentMode`
 * preference. Pure display + dispatch leaf; rendered in the local player's land
 * column beside the undo button.
 */
export function ManualManaToggle() {
  const { t } = useTranslation("game");
  const manualManaOverride = useUiStore((s) => s.manualManaOverride);
  const toggleManualMana = useUiStore((s) => s.toggleManualManaOverride);

  return (
    <button
      type="button"
      onClick={toggleManualMana}
      aria-pressed={manualManaOverride}
      className={`group relative inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[10px] font-medium transition-colors before:absolute before:-inset-2 before:content-[''] ${
        manualManaOverride
          ? "bg-cyan-500/20 text-cyan-200 ring-1 ring-cyan-400/40"
          : "bg-gray-800/80 text-gray-400 hover:bg-gray-700/80 hover:text-gray-200"
      }`}
    >
      {t("mana.manualMana")}
      <GameplayTooltip>{t("mana.manualManaTooltip")}</GameplayTooltip>
    </button>
  );
}
