import { useEffect } from "react";
import { AnimatePresence, motion } from "framer-motion";

import { useMultiplayerStore } from "../../stores/multiplayerStore";

interface ConnectionToastProps {
  onRetry?: () => void;
  onSettings?: () => void;
}

export function ConnectionToast({ onRetry, onSettings }: ConnectionToastProps) {
  const toastMessage = useMultiplayerStore((s) => s.toastMessage);
  const clearToast = useMultiplayerStore((s) => s.clearToast);

  // Auto-dismiss after 5 seconds
  useEffect(() => {
    if (!toastMessage) return;
    const timer = setTimeout(clearToast, 5000);
    return () => clearTimeout(timer);
  }, [toastMessage, clearToast]);

  return (
    <AnimatePresence>
      {toastMessage && (
        <motion.div
          className="fixed bottom-6 left-1/2 z-50 flex -translate-x-1/2 items-center gap-3 rounded-lg bg-gray-900 px-4 py-3 shadow-2xl ring-1 ring-red-700/50"
          initial={{ opacity: 0, y: 20 }}
          animate={{ opacity: 1, y: 0 }}
          exit={{ opacity: 0, y: 20 }}
          transition={{ duration: 0.25 }}
        >
          <span className="text-sm text-gray-200">{toastMessage}</span>
          <div className="flex gap-2">
            {onRetry && (
              <button
                onClick={() => {
                  clearToast();
                  onRetry();
                }}
                className="rounded bg-red-600/80 px-2.5 py-1 text-xs font-semibold text-white transition hover:bg-red-500"
              >
                Retry
              </button>
            )}
            {onSettings && (
              <button
                onClick={() => {
                  clearToast();
                  onSettings();
                }}
                className="rounded bg-gray-700 px-2.5 py-1 text-xs font-semibold text-gray-300 transition hover:bg-gray-600"
              >
                Settings
              </button>
            )}
          </div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
