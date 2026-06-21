import { useEffect, useState } from "react";
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
  const [collapsed, setCollapsed] = useState(false);

  useEffect(() => {
    if (!progress) setCollapsed(false);
  }, [progress]);

  const percent =
    progress && progress.total > 0
      ? Math.min(100, Math.round((progress.resolved / progress.total) * 100))
      : 0;

  return (
    <div className="pointer-events-none fixed inset-0 z-50 flex items-start justify-center">
      <AnimatePresence mode="wait">
        {progress && collapsed && (
          <motion.button
            key="resolution-progress-collapsed"
            type="button"
            className="pointer-events-auto mt-24 flex items-center gap-2 rounded-lg border border-cyan-300/20 bg-black/75 px-3 py-2 text-sm font-medium tabular-nums text-cyan-300 shadow-2xl backdrop-blur-sm transition-colors hover:bg-cyan-950/80"
            initial={{ opacity: 0, y: -10, scale: 0.95 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -10, scale: 0.95 }}
            transition={{ duration: 0.15 }}
            onClick={() => setCollapsed(false)}
            aria-label={t("resolutionProgress.expand")}
          >
            <span>
              {t("resolutionProgress.label", {
                resolved: progress.resolved,
                total: progress.total,
              })}
            </span>
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4">
              <path fillRule="evenodd" d="M5.22 7.22a.75.75 0 0 1 1.06 0L10 10.94l3.72-3.72a.75.75 0 1 1 1.06 1.06l-4.25 4.25a.75.75 0 0 1-1.06 0L5.22 8.28a.75.75 0 0 1 0-1.06Z" clipRule="evenodd" />
            </svg>
          </motion.button>
        )}
        {progress && !collapsed && (
          <motion.div
            key="resolution-progress-expanded"
            className="pointer-events-auto mt-24 flex flex-col items-center gap-2 rounded-lg border border-cyan-300/20 bg-black/75 px-4 py-3 text-cyan-400 shadow-2xl backdrop-blur-sm"
            initial={{ opacity: 0, y: -10, scale: 0.95 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -10, scale: 0.95 }}
            transition={{ duration: 0.2 }}
          >
            <div className="flex w-full items-center justify-between gap-4">
              <span className="text-sm font-medium tabular-nums">
                {t("resolutionProgress.label", {
                  resolved: progress.resolved,
                  total: progress.total,
                })}
              </span>
              <button
                type="button"
                className="rounded-md p-1 text-cyan-300/75 transition-colors hover:bg-white/10 hover:text-cyan-100"
                onClick={() => setCollapsed(true)}
                aria-label={t("resolutionProgress.collapse")}
              >
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4">
                  <path fillRule="evenodd" d="M14.78 12.78a.75.75 0 0 1-1.06 0L10 9.06l-3.72 3.72a.75.75 0 0 1-1.06-1.06l4.25-4.25a.75.75 0 0 1 1.06 0l4.25 4.25a.75.75 0 0 1 0 1.06Z" clipRule="evenodd" />
                </svg>
              </button>
            </div>
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
