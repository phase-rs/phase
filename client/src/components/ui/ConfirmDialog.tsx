import { AnimatePresence, motion } from "framer-motion";
import { useEffect, useId, useRef } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

interface ConfirmDialogProps {
  open: boolean;
  title: string;
  message: string;
  confirmLabel: string;
  onConfirm: () => void;
  onCancel: () => void;
  /** Visual emphasis for the confirm action. */
  tone?: "danger" | "primary";
  /** Optional second confirm action (e.g. import merge vs overwrite). */
  secondaryConfirmLabel?: string;
  onSecondaryConfirm?: () => void;
  secondaryTone?: "danger" | "primary";
}

const CONFIRM_TONE_CLASS = {
  danger:
    "border-rose-400/40 bg-rose-500/20 text-rose-100 hover:bg-rose-500/30 hover:text-white",
  primary:
    "border-sky-400/60 bg-sky-500/14 text-sky-100 hover:bg-sky-500/25 hover:text-white",
} as const;

/**
 * Lightweight confirmation dialog styled to match settings/workspace modals.
 * Portals above `ModalPanelShell` (z-50) so it can stack on nested flows.
 */
export function ConfirmDialog({
  open,
  title,
  message,
  confirmLabel,
  onConfirm,
  onCancel,
  tone = "danger",
  secondaryConfirmLabel,
  onSecondaryConfirm,
  secondaryTone = "primary",
}: ConfirmDialogProps) {
  const { t } = useTranslation();
  const titleId = useId();
  const messageId = useId();
  const cancelRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!open) return;

    const frame = requestAnimationFrame(() => {
      cancelRef.current?.focus();
    });

    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [open, onCancel]);

  return createPortal(
    <AnimatePresence>
      {open && (
        <motion.div
          key="confirm-dialog"
          className="fixed inset-0 z-[70] flex items-center justify-center px-4 py-6"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.18 }}
        >
          <button
            type="button"
            className="absolute inset-0 bg-black/68"
            onClick={onCancel}
            aria-label={t("actions.closeNamed", { name: title })}
          />

          <motion.div
            role="alertdialog"
            aria-modal="true"
            aria-labelledby={titleId}
            aria-describedby={messageId}
            className="relative z-10 w-full max-w-md overflow-hidden rounded-[10px] border border-white/10 bg-[#0b1020] p-6 shadow-[0_18px_48px_rgba(0,0,0,0.48)]"
            initial={{ scale: 0.97, opacity: 0, y: 10 }}
            animate={{ scale: 1, opacity: 1, y: 0 }}
            exit={{ scale: 0.97, opacity: 0, y: 10 }}
            transition={{ duration: 0.2, ease: "easeOut" }}
            onClick={(e) => e.stopPropagation()}
          >
            <h2 id={titleId} className="text-lg font-semibold text-white">
              {title}
            </h2>
            <p id={messageId} className="mt-2 text-sm leading-relaxed text-slate-400">
              {message}
            </p>
            <div className="mt-6 flex flex-wrap justify-end gap-3">
              <button
                ref={cancelRef}
                type="button"
                onClick={onCancel}
                className="rounded-[8px] border border-white/10 bg-slate-950/80 px-4 py-2 text-sm font-medium text-slate-100 transition hover:bg-slate-900"
              >
                {t("common:actions.cancel")}
              </button>
              {secondaryConfirmLabel && onSecondaryConfirm ? (
                <button
                  type="button"
                  onClick={onSecondaryConfirm}
                  className={`rounded-[8px] border px-4 py-2 text-sm font-medium transition ${CONFIRM_TONE_CLASS[secondaryTone]}`}
                >
                  {secondaryConfirmLabel}
                </button>
              ) : null}
              <button
                type="button"
                onClick={onConfirm}
                className={`rounded-[8px] border px-4 py-2 text-sm font-medium transition ${CONFIRM_TONE_CLASS[tone]}`}
              >
                {confirmLabel}
              </button>
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
