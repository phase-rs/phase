import { AnimatePresence, motion } from "framer-motion";

import { menuButtonClass } from "../menu/buttonStyles.ts";

export function ChoiceOverlay({
  title,
  subtitle,
  children,
  widthClassName = "w-full",
  maxWidthClassName = "max-w-6xl",
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
  widthClassName?: string;
  maxWidthClassName?: string;
}) {
  return (
    <div className="fixed inset-0 z-50 flex flex-col px-0 py-0 lg:items-center lg:justify-center lg:px-4 lg:py-6">
      <div className="absolute inset-0 bg-[radial-gradient(circle_at_top,rgba(31,41,55,0.55),rgba(2,6,23,0.92)_58%,rgba(2,6,23,0.98))]" />
      <motion.div
        className={`card-scale-reset relative flex h-full flex-col overflow-hidden border-white/10 bg-[#0b1020]/94 shadow-[0_32px_90px_rgba(0,0,0,0.48)] backdrop-blur-md lg:h-auto lg:max-h-[calc(100vh_-_3rem)] lg:rounded-[28px] lg:border ${widthClassName} ${maxWidthClassName}`}
        initial={{ opacity: 0, y: 18, scale: 0.98 }}
        animate={{ opacity: 1, y: 0, scale: 1 }}
        transition={{ duration: 0.24, ease: "easeOut" }}
      >
        <div className="modal-header-compact shrink-0 border-b border-white/10">
          <div className="modal-eyebrow uppercase tracking-[0.24em] text-slate-500">
            Game Choice
          </div>
          <h2 className="font-semibold text-white">
            {title}
          </h2>
          <p className="modal-subtitle max-w-3xl text-slate-400">
            {subtitle}
          </p>
        </div>
        <div className="flex min-h-0 flex-1 flex-col overflow-y-auto px-2 py-2 lg:px-5 lg:py-5">
          {children}
        </div>
      </motion.div>
    </div>
  );
}

export function ConfirmButton({
  onClick,
  disabled = false,
  label = "Confirm",
}: {
  onClick: () => void;
  disabled?: boolean;
  label?: string;
}) {
  return (
    <AnimatePresence>
      <motion.div
        className="mx-auto w-full max-w-xs shrink-0 px-4 py-1 lg:px-0 lg:py-2"
        initial={{ opacity: 0, y: 20 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ delay: 0.5, duration: 0.3 }}
      >
        <button
          onClick={onClick}
          disabled={disabled}
          className={menuButtonClass({
            tone: "cyan",
            size: "lg",
            disabled,
            className: "w-full",
          })}
        >
          {label}
        </button>
      </motion.div>
    </AnimatePresence>
  );
}
