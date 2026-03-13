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
      className="fixed inset-0 z-50 flex flex-col items-center justify-center px-4"
      style={{
        background:
          "radial-gradient(ellipse at center, rgba(30,30,50,0.95) 0%, rgba(0,0,0,0.98) 70%)",
      }}
    >
      <motion.div
        className="mb-4 text-center sm:mb-8"
        initial={{ opacity: 0, y: -20 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ duration: 0.5 }}
      >
        <h2
          className="text-2xl font-black tracking-wide text-white sm:text-3xl"
          style={{ textShadow: "0 0 20px rgba(200,200,255,0.3)" }}
        >
          {title}
        </h2>
        <p className="mt-2 text-sm text-gray-400">{subtitle}</p>
      </motion.div>
      {children}
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
