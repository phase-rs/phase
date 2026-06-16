import { useEffect } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { useAppNotificationStore } from "../../stores/appToastStore";

/** App-wide notification for menu and other non-multiplayer surfaces. */
export function AppToast() {
  const { t } = useTranslation("common");
  const notification = useAppNotificationStore((s) => s.notification);
  const expiresAt = useAppNotificationStore((s) => s.expiresAt);
  const clearNotification = useAppNotificationStore((s) => s.clearNotification);

  useEffect(() => {
    if (!notification) return;
    const remaining = expiresAt - Date.now();
    const id = setTimeout(clearNotification, Math.max(0, remaining));
    return () => clearTimeout(id);
  }, [notification, expiresAt, clearNotification]);

  return (
    <AnimatePresence>
      {notification && (
        <motion.div
          role="status"
          aria-live="polite"
          aria-atomic="true"
          className="fixed top-4 right-4 z-[60] w-[min(100vw-2rem,22rem)] overflow-hidden rounded-xl border border-white/10 bg-[#0b1020]/96 shadow-[0_16px_48px_rgba(0,0,0,0.45)] backdrop-blur-md"
          initial={{ opacity: 0, x: 24, y: -8 }}
          animate={{ opacity: 1, x: 0, y: 0 }}
          exit={{ opacity: 0, x: 24, y: -8 }}
          transition={{ duration: 0.25 }}
        >
          <div className="border-l-4 border-amber-400/80 px-4 py-3">
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0 flex-1">
                <p className="text-sm font-semibold text-white">
                  {notification.title}
                </p>
                <p className="mt-1 text-sm leading-snug text-slate-400">
                  {notification.description}
                </p>
              </div>
              <button
                type="button"
                onClick={clearNotification}
                aria-label={t("actions.close")}
                className="shrink-0 text-lg leading-none text-slate-500 transition-colors hover:text-slate-300"
              >
                &times;
              </button>
            </div>
          </div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
