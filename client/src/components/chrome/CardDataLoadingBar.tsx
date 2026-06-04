import { AnimatePresence, motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { useCardDataStore } from "../../stores/cardDataStore";

/**
 * Persistent chrome affordance for the shared card-database warm. While the
 * card DB is loading the menu gates deck-requiring actions (Play vs AI / Online
 * disable); this restores the visible "preparing card data" signal the legacy
 * menu cards used to show, as a slim top progress bar + label.
 *
 * The warm reports only a lifecycle status (no byte progress), so the bar is
 * intentionally indeterminate — a sweeping segment rather than a false
 * percentage. Mounted once in the AppShell so it covers every menu surface.
 */
export function CardDataLoadingBar() {
  const { t } = useTranslation("menu");
  const status = useCardDataStore((s) => s.status);

  return (
    <AnimatePresence>
      {status === "loading" && (
        <motion.div
          key="card-data-loading"
          initial={{ opacity: 0, y: -4 }}
          animate={{ opacity: 1, y: 0 }}
          exit={{ opacity: 0, y: -4 }}
          transition={{ duration: 0.2 }}
          className="pointer-events-none fixed inset-x-0 top-0 z-[60]"
          role="status"
          aria-live="polite"
          aria-label={t("home.preparingCards")}
        >
          {/* Indeterminate track — thick + glowing so it reads at a glance. */}
          <div className="relative h-[4px] w-full overflow-hidden bg-arcane/10 shadow-[0_0_14px_rgba(56,189,248,0.5)]">
            <motion.div
              className="absolute inset-y-0 w-2/5 bg-gradient-to-r from-transparent via-arcane to-transparent"
              animate={{ x: ["-110%", "260%"] }}
              transition={{ duration: 1.05, repeat: Infinity, ease: "easeInOut" }}
            />
          </div>
          {/* Prominent labelled pill with a spinner. */}
          <div className="flex justify-center">
            <span className="mt-3 inline-flex items-center gap-2.5 rounded-full border border-arcane/30 bg-black/75 px-4 py-2 text-xs font-semibold tracking-wide text-arcane-text shadow-[0_8px_30px_rgba(0,0,0,0.55)] backdrop-blur-md">
              <span className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-arcane/30 border-t-arcane" />
              {t("home.preparingCards")}
            </span>
          </div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
