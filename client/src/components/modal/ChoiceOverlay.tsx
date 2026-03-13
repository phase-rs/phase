import { AnimatePresence, motion } from "framer-motion";

import { menuButtonClass } from "../menu/buttonStyles.ts";

export function ChoiceOverlay({
  title,
  subtitle,
  children,
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <div
      className="fixed inset-0 z-50 overflow-y-auto px-3 py-4 sm:px-4 sm:py-6"
      style={{
        background:
          "radial-gradient(ellipse at center, rgba(30,30,50,0.95) 0%, rgba(0,0,0,0.98) 70%)",
      }}
    >
      <div className="flex min-h-full flex-col items-center justify-center pb-[env(safe-area-inset-bottom)] pt-[env(safe-area-inset-top)]">
        <motion.div
          className="mb-4 text-center sm:mb-8"
          initial={{ opacity: 0, y: -20 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.5 }}
        >
          <h2
            className="text-xl font-black tracking-wide text-white sm:text-3xl"
            style={{ textShadow: "0 0 20px rgba(200,200,255,0.3)" }}
          >
            {title}
          </h2>
          <p className="mt-2 max-w-xl text-xs text-gray-400 sm:text-sm">{subtitle}</p>
        </motion.div>
        {children}
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
        className="w-full max-w-xs px-4 sm:px-0"
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
