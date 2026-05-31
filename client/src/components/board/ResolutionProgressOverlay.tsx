import { AnimatePresence, motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { useGameStore } from "../../stores/gameStore";

/**
 * Non-blocking overlay shown while a large stack-resolution storm drains
 * (e.g. a Scute Swarm landfall cascade resolved via "Resolve All"). It renders
 * the engine-provided `resolved`/`total` counts from
 * `gameStore.resolutionProgress`, which `dispatchResolveAll` sets per chunk and
 * clears when the drain finishes; it renders nothing when no storm is in
 * flight. Display-only — it drives no game state and derives nothing; the bar
 * width is pure presentation of the two engine counts.
 */
export function ResolutionProgressOverlay() {
  const { t } = useTranslation("game");
  const progress = useGameStore((s) => s.resolutionProgress);

  const percent =
    progress && progress.total > 0
      ? Math.min(100, Math.round((progress.resolved / progress.total) * 100))
      : 0;

  return (
    <div className="pointer-events-none fixed inset-0 z-50 flex items-start justify-center">
      <AnimatePresence>
        {progress && (
          <motion.div
            className="mt-24 flex flex-col items-center gap-2 rounded-xl bg-black/75 px-6 py-4 text-cyan-400 backdrop-blur-sm"
            initial={{ opacity: 0, y: -10, scale: 0.95 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -10, scale: 0.95 }}
            transition={{ duration: 0.2 }}
          >
            <span className="text-sm font-medium tabular-nums">
              {t("resolutionProgress.label", {
                resolved: progress.resolved,
                total: progress.total,
              })}
            </span>
            <div className="h-1.5 w-48 overflow-hidden rounded-full bg-cyan-950">
              <motion.div
                className="h-full rounded-full bg-cyan-400"
                animate={{ width: `${percent}%` }}
                transition={{ duration: 0.15, ease: "linear" }}
              />
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}
