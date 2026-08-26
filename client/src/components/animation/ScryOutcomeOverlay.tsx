import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { getOpponentDisplayName } from "../../stores/multiplayerStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";

/**
 * Brief, board-visible confirmation of a completed scry. The engine event
 * supplies the public placement counts; this component only presents them.
 */
export function ScryOutcomeOverlay() {
  const outcome = useUiStore((state) => state.scryOutcome);
  const playerId = usePlayerId();
  const shouldReduceMotion = useReducedMotion();
  const { t } = useTranslation();

  const player = outcome
    ? outcome.playerId === playerId
      ? t("scryOutcome.you")
      : getOpponentDisplayName(outcome.playerId)
    : "";

  return (
    <AnimatePresence>
      {outcome && (
        <motion.div
          className="pointer-events-none fixed top-[max(env(safe-area-inset-top),0.75rem)] left-1/2 z-[52] -translate-x-1/2"
          role="status"
          aria-live="polite"
          initial={shouldReduceMotion ? { opacity: 0 } : { opacity: 0, y: -12, scale: 0.96 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          exit={shouldReduceMotion ? { opacity: 0 } : { opacity: 0, y: -8, scale: 0.98 }}
          transition={{ duration: shouldReduceMotion ? 0.1 : 0.22 }}
        >
          <div className="min-w-56 rounded-xl border border-sky-300/45 bg-slate-950/90 px-4 py-3 text-center shadow-[0_0_28px_rgba(56,189,248,0.24)] backdrop-blur-md">
            <p className="text-xs font-semibold uppercase tracking-[0.2em] text-sky-200">
              {t("scryOutcome.title")}
            </p>
            <p className="mt-1 text-sm font-medium text-slate-100" data-testid="scry-outcome">
              {t("scryOutcome.result", {
                player,
                top: outcome.topCount,
                bottom: outcome.bottomCount,
              })}
            </p>
          </div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
