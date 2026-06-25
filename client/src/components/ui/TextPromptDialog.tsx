import { AnimatePresence, motion } from "framer-motion";
import { useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

import { menuButtonClass } from "../menu/buttonStyles";

interface TextPromptDialogProps {
  open: boolean;
  title: string;
  label: string;
  initialValue?: string;
  placeholder?: string;
  confirmLabel: string;
  maxLength?: number;
  onConfirm: (value: string) => void;
  onCancel: () => void;
}

/**
 * Site-styled replacement for `window.prompt()` — single-line text entry with
 * Cancel / confirm actions. Used by deck-library folder flows and any other
 * menu surfaces that need a short name without pulling in a full modal shell.
 */
export function TextPromptDialog({
  open,
  title,
  label,
  initialValue = "",
  placeholder,
  confirmLabel,
  maxLength,
  onConfirm,
  onCancel,
}: TextPromptDialogProps) {
  const { t } = useTranslation();
  const titleId = useId();
  const labelId = useId();
  const inputRef = useRef<HTMLInputElement>(null);
  const [value, setValue] = useState(initialValue);

  useEffect(() => {
    if (!open) return;
    setValue(initialValue);
    const frame = requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.select();
    });
    return () => cancelAnimationFrame(frame);
  }, [open, initialValue]);

  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onCancel]);

  const trimmed = value.trim();
  const canConfirm = trimmed.length > 0;

  const handleSubmit = (event: React.FormEvent) => {
    event.preventDefault();
    if (!canConfirm) return;
    onConfirm(trimmed);
  };

  return createPortal(
    <AnimatePresence>
      {open && (
        <motion.div
          className="fixed inset-0 z-50 flex items-center justify-center px-4"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.18 }}
        >
          <button
            type="button"
            className="absolute inset-0 bg-black/68 backdrop-blur-[2px]"
            onClick={onCancel}
            aria-label={t("actions.closeNamed", { name: title })}
          />

          <motion.div
            role="dialog"
            aria-modal="true"
            aria-labelledby={titleId}
            className="relative z-10 w-full max-w-md rounded-[22px] border border-white/10 bg-[#0b1020]/96 p-6 shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md"
            initial={{ scale: 0.97, opacity: 0, y: 10 }}
            animate={{ scale: 1, opacity: 1, y: 0 }}
            exit={{ scale: 0.97, opacity: 0, y: 10 }}
            transition={{ duration: 0.2, ease: "easeOut" }}
            onClick={(event) => event.stopPropagation()}
          >
            <h2 id={titleId} className="text-base font-semibold text-white lg:text-lg">
              {title}
            </h2>

            <form onSubmit={handleSubmit} className="mt-4">
              <label id={labelId} htmlFor={`${labelId}-input`} className="text-sm text-slate-300">
                {label}
              </label>
              <input
                id={`${labelId}-input`}
                ref={inputRef}
                type="text"
                value={value}
                maxLength={maxLength}
                placeholder={placeholder}
                onChange={(event) => setValue(event.target.value)}
                className="mt-2 w-full rounded-xl border border-white/20 bg-white/8 px-3 py-2.5 text-sm text-white placeholder-white/30 outline-none backdrop-blur-sm transition-colors focus:border-cyan-300/60 focus:ring-1 focus:ring-cyan-300/30"
                aria-labelledby={labelId}
              />

              <div className="mt-5 flex justify-end gap-2">
                <button
                  type="button"
                  onClick={onCancel}
                  className={menuButtonClass({ tone: "neutral", size: "sm" })}
                >
                  {t("actions.cancel")}
                </button>
                <button
                  type="submit"
                  disabled={!canConfirm}
                  className={menuButtonClass({
                    tone: "cyan",
                    size: "sm",
                    disabled: !canConfirm,
                  })}
                >
                  {confirmLabel}
                </button>
              </div>
            </form>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
