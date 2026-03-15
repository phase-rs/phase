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
    <div className="fixed inset-0 z-50 overflow-y-auto px-3 py-4 sm:px-4 sm:py-6">
      <div className="absolute inset-0 bg-[radial-gradient(circle_at_top,rgba(31,41,55,0.55),rgba(2,6,23,0.92)_58%,rgba(2,6,23,0.98))]" />
      <div className="relative flex min-h-full flex-col items-center justify-center pb-[env(safe-area-inset-bottom)] pt-[env(safe-area-inset-top)]">
        <motion.div
          className={`${widthClassName} overflow-hidden rounded-[28px] border border-white/10 bg-[#0b1020]/94 shadow-[0_32px_90px_rgba(0,0,0,0.48)] backdrop-blur-md ${maxWidthClassName}`}
          initial={{ opacity: 0, y: 18, scale: 0.98 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          transition={{ duration: 0.24, ease: "easeOut" }}
        >
          <div className="border-b border-white/10 px-5 py-5 sm:px-7 sm:py-6">
            <div className="text-[0.68rem] uppercase tracking-[0.24em] text-slate-500">
              Game Choice
            </div>
            <h2 className="mt-2 text-2xl font-semibold text-white sm:text-3xl">
              {title}
            </h2>
            <p className="mt-2 max-w-3xl text-sm text-slate-400 sm:text-[0.95rem]">
              {subtitle}
            </p>
          </div>
          <div className="px-3 py-4 sm:px-5 sm:py-5">
            {children}
          </div>
        </motion.div>
      </div>
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
        className="mx-auto w-full max-w-xs px-4 sm:px-0"
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
