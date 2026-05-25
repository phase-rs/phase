import { motion, AnimatePresence } from "framer-motion";
import { useTranslation } from "react-i18next";

interface PausedBannerProps {
  isVisible: boolean;
  reason: string;
}

/**
 * Top-center banner shown to all players (host AND guests) while the game is
 * paused due to a disconnect or host-initiated pause. Hosts may also see the
 * `DisconnectChoiceDialog` overlay simultaneously; guests see only this
 * banner.
 */
export function PausedBanner({ isVisible, reason }: PausedBannerProps) {
  const { t } = useTranslation();
  return (
    <AnimatePresence>
      {isVisible && (
        <motion.div
          className="pointer-events-none fixed inset-x-0 top-4 z-40 flex justify-center"
          initial={{ opacity: 0, y: -16 }}
          animate={{ opacity: 1, y: 0 }}
          exit={{ opacity: 0, y: -16 }}
          transition={{ duration: 0.18 }}
        >
          <div className="rounded-full bg-amber-500/20 px-4 py-1.5 text-xs font-semibold text-amber-200 ring-1 ring-amber-300/40 backdrop-blur">
            {t("pausedBanner.message", { reason })}
          </div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
