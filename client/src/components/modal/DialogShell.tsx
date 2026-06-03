import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import { useEffect, type ReactNode } from "react";
import { useTranslation } from "react-i18next";

import type { ObjectId } from "../../adapter/types.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { useOptionalDialogPeek } from "./dialogPeekContext.ts";

interface DialogShellProps {
  eyebrow?: ReactNode;
  eyebrowClassName?: string;
  title: ReactNode;
  subtitle?: ReactNode;
  size?: "sm" | "md" | "lg";
  scrollable?: boolean;
  children: ReactNode;
  footer?: ReactNode;
  onClose?: () => void;
  /** When set, hovering anywhere on the dialog card fires inspectObject for
   * the referenced game object. Use this for dialogs that represent a single
   * card subject (cast prompts, face choices, miracle reveal, etc.). */
  previewObjectId?: ObjectId;
}

const SIZE_CLASS: Record<NonNullable<DialogShellProps["size"]>, string> = {
  sm: "max-w-sm",
  md: "max-w-md",
  lg: "max-w-3xl",
};

export function DialogShell({
  eyebrow,
  eyebrowClassName,
  title,
  subtitle,
  size = "sm",
  scrollable = false,
  children,
  footer,
  onClose,
  previewObjectId,
}: DialogShellProps) {
  const { t } = useTranslation("game");
  const peek = useOptionalDialogPeek();
  const inspectHoverProps = useInspectHoverProps();
  const resolvedEyebrow = eyebrow ?? t("dialogShell.eyebrow");
  const cardHoverProps =
    previewObjectId != null ? inspectHoverProps(previewObjectId) : undefined;

  // Esc-to-close: standard modal contract. Only attach when the dialog is
  // dismissable (consumers like ChoiceOverlay that omit `onClose` have a
  // different dismissal model and shouldn't intercept Escape).
  useEffect(() => {
    if (!onClose) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

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
          <div {...cardHoverProps} className={cardClass}>
            <DialogHeader
              eyebrow={resolvedEyebrow}
              eyebrowClassName={eyebrowClassName}
              title={title}
              subtitle={subtitle}
            />
            {onClose ? <CloseButton onClose={onClose} /> : null}
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
 * Pill tab attached to the edge the dialog slides toward when peeked. The
 * pulsing glow signals "actionable affordance — click me to peek." Mirrors
 * `PeekRestoreTab`'s `direction` axis so the collapse cue points the same way
 * the modal exits: the right edge on wide viewports, the bottom edge on narrow
 * ones (where the dialog slides down rather than sideways).
 */
export function PeekTab({
  onClick,
  direction = "right",
}: {
  onClick: () => void;
  direction?: "right" | "bottom";
}) {
  const { t } = useTranslation("game");
  const shouldReduceMotion = useReducedMotion();

  // Glow is offset toward the edge the modal slides to (+x right / +y bottom)
  // so it visually radiates toward the battlefield the player wants to peek at.
  const restingShadow =
    direction === "right"
      ? "0 18px 36px rgba(0,0,0,0.55), 14px 0 0 -8px rgba(34,211,238,0)"
      : "0 18px 36px rgba(0,0,0,0.55), 0 14px 0 -8px rgba(34,211,238,0)";
  const pulseShadow =
    direction === "right"
      ? "0 18px 36px rgba(0,0,0,0.55), 18px 0 36px rgba(34,211,238,0.65)"
      : "0 18px 36px rgba(0,0,0,0.55), 0 18px 36px rgba(34,211,238,0.65)";

  const positionClass =
    direction === "right"
      ? "right-0 top-1/2 h-24 w-9 -translate-y-1/2 translate-x-1/3"
      : "bottom-0 left-1/2 h-9 w-24 -translate-x-1/2 translate-y-1/3";

  // The chevron points the way the modal exits: right as-is, down when rotated.
  const iconClass =
    direction === "right"
      ? "group-hover:translate-x-0.5"
      : "rotate-90 group-hover:translate-y-0.5";

  return (
    <motion.button
      type="button"
      onClick={onClick}
      aria-label={t("dialogShell.peekAria")}
      title={t("dialogShell.peekTitle")}
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
      className={`group absolute z-20 flex items-center justify-center rounded-2xl border border-cyan-400/50 bg-[#0b1020]/96 text-cyan-200 backdrop-blur-md transition-colors hover:border-cyan-300 hover:bg-cyan-500/20 hover:text-white ${positionClass}`}
    >
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 20 20"
        fill="currentColor"
        className={`h-6 w-6 transition-transform ${iconClass}`}
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

/**
 * X close affordance in the dialog's top-right corner. Sits inside the card
 * (so it scrolls with content if the dialog is scrollable) at z-20 to ride
 * above the gradient overlay but below the peek tab. Renders only when the
 * caller provides `onClose` — non-dismissable dialogs (ChoiceOverlay) don't
 * get one.
 */
function CloseButton({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation("game");
  return (
    <button
      type="button"
      onClick={onClose}
      aria-label={t("dialogShell.close")}
      title={t("dialogShell.closeTitle")}
      className="absolute right-2 top-2 z-20 flex h-8 w-8 items-center justify-center rounded-full text-slate-400 transition hover:bg-white/10 hover:text-white focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/40 lg:right-3 lg:top-3"
    >
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 20 20"
        fill="currentColor"
        className="h-4 w-4"
        aria-hidden
      >
        <path
          fillRule="evenodd"
          d="M4.28 4.22a.75.75 0 0 1 1.06 0L10 8.94l4.66-4.72a.75.75 0 1 1 1.06 1.06L11.06 10l4.66 4.72a.75.75 0 1 1-1.06 1.06L10 11.06l-4.66 4.72a.75.75 0 0 1-1.06-1.06L8.94 10 4.28 5.28a.75.75 0 0 1 0-1.06Z"
          clipRule="evenodd"
        />
      </svg>
    </button>
  );
}
