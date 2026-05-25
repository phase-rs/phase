import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { menuButtonClass } from "../menu/buttonStyles";

interface JoinErrorDialogProps {
  title: string;
  message: string;
  /** Optional action button. When set, a primary-tone button appears with
   * this label and fires the callback. Used for recoverable errors like
   * build-commit mismatch where the user can refresh to resolve. */
  primaryAction?: { label: string; onClick: () => void };
  onDismiss: () => void;
}

/**
 * Modal surface for fatal guest-side errors that need more prominence
 * than a toast — in particular `build_mismatch`, which requires the user
 * to refresh the page to pick up a new client build. Kept deliberately
 * small and parameterized so one component covers the category rather
 * than spawning a dialog per error reason.
 */
export function JoinErrorDialog({
  title,
  message,
  primaryAction,
  onDismiss,
}: JoinErrorDialogProps) {
  const { t } = useTranslation("multiplayer");
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/70" onClick={onDismiss} />
      <motion.div
        initial={{ opacity: 0, scale: 0.96 }}
        animate={{ opacity: 1, scale: 1 }}
        transition={{ duration: 0.18 }}
        className="relative z-10 w-full max-w-md rounded-[22px] border border-white/10 bg-[#0b1020]/96 p-6 shadow-2xl backdrop-blur-md"
      >
        <h2 className="text-base font-semibold text-white">{title}</h2>
        <p className="mt-3 text-sm leading-6 text-slate-300">{message}</p>
        <div className="mt-5 flex justify-end gap-2">
          <button
            type="button"
            onClick={onDismiss}
            className={menuButtonClass({ tone: "neutral", size: "sm" })}
          >
            {t("joinErrorDialog.dismiss")}
          </button>
          {primaryAction && (
            <button
              type="button"
              onClick={primaryAction.onClick}
              className={menuButtonClass({ tone: "cyan", size: "sm" })}
            >
              {primaryAction.label}
            </button>
          )}
        </div>
      </motion.div>
    </div>
  );
}
