import { AnimatePresence, motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { useGameStore } from "../../stores/gameStore";
import { LogEntry } from "../log/LogEntry";

/**
 * One-shot display of the engine-authored transition completed while restoring
 * a saved game. It never dispatches resolution work or derives a progress
 * denominator: the presentation is already bounded by the engine.
 */
export function ResolutionProgressOverlay() {
  const { t } = useTranslation("game");
  const presentation = useGameStore((s) => s.restoredStackAutomation);
  const dismiss = useGameStore((s) => s.dismissRestoredStackAutomation);

  return (
    <div className="pointer-events-none fixed inset-0 z-50 flex items-start justify-center">
      <AnimatePresence>
        {presentation && (
          <motion.section
            key="restored-stack-automation"
            className="pointer-events-auto mt-24 w-[min(28rem,calc(100vw-2rem))] rounded-lg border border-cyan-300/20 bg-black/80 px-4 py-3 text-cyan-100 shadow-2xl backdrop-blur-sm"
            initial={{ opacity: 0, y: -10, scale: 0.95 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -10, scale: 0.95 }}
            transition={{ duration: 0.2 }}
            aria-live="polite"
          >
            <div className="flex items-start justify-between gap-4">
              <div>
                <h2 className="text-sm font-semibold">
                  {t(`restoredAutomation.${presentation.outcome}.title`)}
                </h2>
                <p className="mt-1 text-xs text-cyan-100/80">
                  {t(`restoredAutomation.${presentation.outcome}.summary`, {
                    count: presentation.automatedResolutionCount,
                    omitted: presentation.omittedEventCount,
                  })}
                </p>
              </div>
              <button
                type="button"
                className="min-h-11 min-w-11 rounded-md px-2 py-1 text-xs font-medium text-cyan-200 transition hover:bg-white/10"
                onClick={dismiss}
              >
                {t("restoredAutomation.dismiss")}
              </button>
            </div>
            {presentation.logEntries.length > 0 && (
              <div className="mt-3 max-h-36 overflow-y-auto rounded border border-white/10 bg-slate-950/60 px-2 py-1">
                {presentation.logEntries.map((entry, index) => (
                  <LogEntry key={`${entry.seq}-${index}`} entry={entry} />
                ))}
              </div>
            )}
          </motion.section>
        )}
      </AnimatePresence>
    </div>
  );
}
