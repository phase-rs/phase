import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import type { ReactNode } from "react";

import { useOptionalDialogPeek } from "./dialogPeekContext.ts";

interface DialogShellProps {
  eyebrow?: ReactNode;
  eyebrowClassName?: string;
  title: ReactNode;
  subtitle?: ReactNode;
  size?: "sm" | "md";
  scrollable?: boolean;
  children: ReactNode;
  footer?: ReactNode;
  onClose?: () => void;
}

const SIZE_CLASS: Record<NonNullable<DialogShellProps["size"]>, string> = {
  sm: "max-w-sm",
  md: "max-w-md",
};

export function DialogShell({
  eyebrow = "Game Choice",
  eyebrowClassName,
  title,
  subtitle,
  size = "sm",
  scrollable = false,
  children,
  footer,
  onClose,
}: DialogShellProps) {
  const peek = useOptionalDialogPeek();

  const cardClass = [
    "relative z-10 w-full overflow-hidden rounded-[16px] lg:rounded-[24px] border border-white/10 bg-[#0b1020]/96 shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md",
    scrollable
      ? "max-h-[calc(100vh_-_2rem_-_env(safe-area-inset-top)_-_env(safe-area-inset-bottom))] overflow-y-auto"
      : "",
  ]
    .filter(Boolean)
    .join(" ");

  // Wrapper class controls dialog width AND provides the positioning
  // context for the peek tab, which sits at the wrapper's right edge so
  // it stays attached to the card (which clips its own overflow).
  const wrapperClass = ["relative z-10 w-full", SIZE_CLASS[size]].join(" ");

  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-0 z-50 flex items-center justify-center px-2 py-2 lg:px-4 lg:py-6"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.2 }}
      >
        <div
          className="absolute inset-0 bg-black/60"
          onClick={onClose}
          aria-hidden="true"
        />

        <motion.div
          className={wrapperClass}
          initial={{ scale: 0.95, opacity: 0, y: 10 }}
          animate={{ scale: 1, opacity: 1, y: 0 }}
          exit={{ scale: 0.95, opacity: 0, y: 10 }}
          transition={{ duration: 0.2, ease: "easeOut" }}
        >
          <div className={cardClass}>
            <DialogHeader
              eyebrow={eyebrow}
              eyebrowClassName={eyebrowClassName}
              title={title}
              subtitle={subtitle}
            />
            {children}
            {footer ? (
              <div className="border-t border-white/5 px-3 py-3 lg:px-5 lg:py-4">
                {footer}
              </div>
            ) : null}
          </div>
          {peek ? <PeekTab onClick={peek.togglePeek} /> : null}
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}

interface DialogHeaderProps {
  eyebrow: ReactNode;
  eyebrowClassName?: string;
  title: ReactNode;
  subtitle?: ReactNode;
}

export function DialogHeader({
  eyebrow,
  eyebrowClassName,
  title,
  subtitle,
}: DialogHeaderProps) {
  const eyebrowClass = [
    "text-[0.68rem] uppercase tracking-[0.22em]",
    eyebrowClassName ?? "text-slate-500",
  ].join(" ");

  return (
    <div className="relative border-b border-white/10 px-3 py-3 lg:px-5 lg:py-5">
      <div className={eyebrowClass}>{eyebrow}</div>
      <h2 className="mt-1 text-base font-semibold text-white lg:text-xl">
        {title}
      </h2>
      {subtitle ? (
        <p className="mt-1 text-xs text-slate-400 lg:text-sm">{subtitle}</p>
      ) : null}
    </div>
  );
}

/**
 * Vertical pill tab attached to the right edge of a dialog. Pulsing
 * right-side glow signals "actionable affordance — click me to peek."
 * Mirrors the stack panel's collapse pattern but with stronger CTA styling
 * since the dialog is blocking content the player likely wants to see.
 */
export function PeekTab({ onClick }: { onClick: () => void }) {
  const shouldReduceMotion = useReducedMotion();

  // Glow is offset to the right (+x in box-shadow) so it visually radiates
  // toward the battlefield the player wants to peek at — directional cue.
  const restingShadow = "0 18px 36px rgba(0,0,0,0.55), 14px 0 0 -8px rgba(34,211,238,0)";
  const pulseShadow = "0 18px 36px rgba(0,0,0,0.55), 18px 0 36px rgba(34,211,238,0.65)";

  return (
    <motion.button
      type="button"
      onClick={onClick}
      aria-label="Move dialog out of the way"
      title="Peek at the battlefield"
      animate={
        shouldReduceMotion
          ? undefined
          : { boxShadow: [restingShadow, pulseShadow, restingShadow] }
      }
      transition={
        shouldReduceMotion
          ? undefined
          : { duration: 2.4, repeat: Infinity, ease: "easeInOut" }
      }
      className="group absolute right-0 top-1/2 z-20 flex h-24 w-9 -translate-y-1/2 translate-x-1/3 items-center justify-center rounded-2xl border border-cyan-400/50 bg-[#0b1020]/96 text-cyan-200 backdrop-blur-md transition-colors hover:border-cyan-300 hover:bg-cyan-500/20 hover:text-white"
    >
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 20 20"
        fill="currentColor"
        className="h-6 w-6 transition-transform group-hover:translate-x-0.5"
      >
        <path
          fillRule="evenodd"
          d="M7.22 4.22a.75.75 0 0 1 1.06 0l5.25 5.25a.75.75 0 0 1 0 1.06l-5.25 5.25a.75.75 0 1 1-1.06-1.06L11.94 10 7.22 5.28a.75.75 0 0 1 0-1.06Z"
          clipRule="evenodd"
        />
      </svg>
    </motion.button>
  );
}

/** Backwards-compatible alias for ChoiceOverlay's existing import site. */
export const PeekButton = PeekTab;
