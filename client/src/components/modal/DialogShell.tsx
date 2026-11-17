import { AnimatePresence, motion, useDragControls, useReducedMotion } from "framer-motion";
import {
  useEffect,
  useId,
  useRef,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";

import type { ObjectId } from "../../adapter/types.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { useOptionalDialogPeek } from "./dialogPeekContext.ts";
import { useIsNarrowViewport } from "./DialogHost.tsx";

interface DialogShellProps {
  eyebrow?: ReactNode;
  eyebrowClassName?: string;
  title: ReactNode;
  subtitle?: ReactNode;
  size?: "sm" | "md" | "lg";
  /** The dialog's body: the region that scrolls, between the pinned header
   * and footer, when the dialog is taller than the screen. */
  children: ReactNode;
  /** Pinned below the body, so it stays in view however tall the body grows.
   * The place for the dialog's actions. */
  footer?: ReactNode;
  onClose?: () => void;
  /** When set, hovering anywhere on the dialog card fires inspectObject for
   * the referenced game object. Use this for dialogs that represent a single
   * card subject (cast prompts, face choices, miracle reveal, etc.). */
  previewObjectId?: ObjectId;
}

// The overlay's gutter, which the card sits inside at rest; a drag by the
// header can still carry it into the gutter. Each side also clears that
// side's safe-area inset: under `viewport-fit=cover`, a landscape phone's
// notch or camera cutout sits inside the left or right inset, and a wide
// dialog would otherwise rest under it.
const OVERLAY_CLASS = [
  "fixed inset-0 z-50 flex py-2 lg:py-6",
  "pl-[max(env(safe-area-inset-left),0.5rem)] pr-[max(env(safe-area-inset-right),0.5rem)]",
  "lg:pl-[max(env(safe-area-inset-left),1rem)] lg:pr-[max(env(safe-area-inset-right),1rem)]",
].join(" ");

const SIZE_CLASS: Record<NonNullable<DialogShellProps["size"]>, string> = {
  sm: "max-w-sm",
  md: "max-w-md",
  lg: "max-w-3xl",
};

// The card is a column that never outgrows the screen: the header and the
// `footer` slot stay pinned and only the body scrolls, so a tall dialog keeps
// its title and its footer in view. `100dvh` follows the mobile URL bar where
// `100vh` does not. The rem terms are the overlay's vertical padding, top plus
// bottom (`py-2`, `lg:py-6`), and the safe-area insets come off as well, so a
// card at its cap always fits inside the overlay.
//
// The card can scroll too, as a last resort: on a screen so short that the
// header and footer leave the body less than its one-row floor, the column
// overflows and the card scrolls it whole, footer included. The card also
// owns `overscroll-contain`, which stops a scroll that runs past the end of
// the body whenever the card itself scrolls, and on Chromium 144+ even when
// it has nothing to scroll. In game, the viewport lock keeps the board still
// regardless.
const CARD_CLASS = [
  "relative z-10 flex w-full flex-col overflow-y-auto overflow-x-hidden overscroll-contain thin-scrollbar",
  "rounded-[16px] lg:rounded-[24px] border border-white/10 bg-[#0b1020]/96",
  "shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md",
  "max-h-[calc(100dvh_-_1rem_-_env(safe-area-inset-top)_-_env(safe-area-inset-bottom))]",
  "lg:max-h-[calc(100dvh_-_3rem_-_env(safe-area-inset-top)_-_env(safe-area-inset-bottom))]",
].join(" ");

// The scroll region between the pinned header and footer. `min-h-11` keeps it
// at least one tap target tall, however tall the header and footer grow.
// `relative` makes the body the containing block for an absolutely positioned
// child (an `sr-only` label, say), so the child scrolls with the body; with
// the card as its containing block, it would stretch the card's scroll range
// and let the pinned header and footer scroll away. `overflow-x-hidden` clips
// a wide child as the card does (`overflow-y-auto` alone turns x to `auto` and
// would grow a horizontal scrollbar). The body leaves `overscroll-contain` to
// the card: Chromium 144+ applies it even to a container with nothing left to
// scroll, so here it would stop a swipe on the body from reaching the card.
const BODY_CLASS = "relative min-h-11 flex-1 overflow-y-auto overflow-x-hidden thin-scrollbar";

export function DialogShell({
  eyebrow,
  eyebrowClassName,
  title,
  subtitle,
  size = "sm",
  children,
  footer,
  onClose,
  previewObjectId,
}: DialogShellProps) {
  const { t } = useTranslation("game");
  const titleId = useId();
  const peek = useOptionalDialogPeek();
  const isNarrow = useIsNarrowViewport();
  const inspectHoverProps = useInspectHoverProps();
  const resolvedEyebrow = eyebrow ?? t("dialogShell.eyebrow");
  const cardHoverProps =
    previewObjectId != null ? inspectHoverProps(previewObjectId) : undefined;

  // Drag-to-reposition: the dialog can sit over board content the player needs
  // to see (targets, the mana they're paying with). `dragListener={false}` +
  // `dragControls` mean a drag only begins from the header handle, so clicks on
  // body controls never start a drag. Constrained to the overlay so the card
  // can't be flung off-screen. Position resets on each open (fresh mount).
  const dragControls = useDragControls();
  const constraintsRef = useRef<HTMLDivElement>(null);
  const startHeaderDrag = (event: ReactPointerEvent) => dragControls.start(event);

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

  // Wrapper class controls dialog width AND provides the positioning
  // context for the peek tab, which sits at the wrapper's right edge so
  // it stays attached to the card (which clips its own overflow). The wrapper
  // centres itself with auto margins: unlike `items-center`, they fall to zero
  // when the card is taller than the overlay, so an oversized card starts at
  // the top instead of spilling off both edges.
  const wrapperClass = ["relative z-10 m-auto w-full", SIZE_CLASS[size]].join(" ");

  return (
    <AnimatePresence>
      <motion.div
        ref={constraintsRef}
        className={OVERLAY_CLASS}
        data-card-preview-dock="side"
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
          role="dialog"
          aria-labelledby={titleId}
          initial={{ scale: 0.95, opacity: 0, y: 10 }}
          animate={{ scale: 1, opacity: 1, y: 0 }}
          exit={{ scale: 0.95, opacity: 0, y: 10 }}
          transition={{ duration: 0.2, ease: "easeOut" }}
          drag
          dragListener={false}
          dragControls={dragControls}
          dragMomentum={false}
          dragElastic={0.05}
          dragConstraints={constraintsRef}
        >
          <div {...cardHoverProps} className={CARD_CLASS}>
            <DialogHeader
              eyebrow={resolvedEyebrow}
              eyebrowClassName={eyebrowClassName}
              title={title}
              titleId={titleId}
              subtitle={subtitle}
              onHandlePointerDown={startHeaderDrag}
            />
            {onClose ? <CloseButton onClose={onClose} /> : null}
            <div className={BODY_CLASS}>{children}</div>
            {footer ? (
              <div className="shrink-0 border-t border-white/5 px-3 py-3 lg:px-5 lg:py-4">
                {footer}
              </div>
            ) : null}
          </div>
          {peek ? (
            <PeekTab
              onClick={peek.togglePeek}
              direction={isNarrow ? "bottom" : "right"}
            />
          ) : null}
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}

interface DialogHeaderProps {
  eyebrow: ReactNode;
  eyebrowClassName?: string;
  title: ReactNode;
  titleId: string;
  subtitle?: ReactNode;
  /** When provided, the header acts as the dialog's drag handle: pointer-down
   * starts a drag via the shell's `dragControls`. Absent → static header. */
  onHandlePointerDown?: (event: ReactPointerEvent) => void;
}

export function DialogHeader({
  eyebrow,
  eyebrowClassName,
  title,
  titleId,
  subtitle,
  onHandlePointerDown,
}: DialogHeaderProps) {
  const eyebrowClass = [
    "text-[0.68rem] uppercase tracking-[0.22em]",
    eyebrowClassName ?? "text-slate-500",
  ].join(" ");

  // `touch-none` keeps a touch-drag on the handle from scrolling the page;
  // `cursor-grab` signals the affordance. Only applied when draggable.
  const handleClass = onHandlePointerDown
    ? "cursor-grab touch-none select-none active:cursor-grabbing"
    : "";

  return (
    <div
      onPointerDown={onHandlePointerDown}
      className={`relative shrink-0 border-b border-white/10 px-3 py-3 lg:px-5 lg:py-5 ${handleClass}`}
    >
      <div className={eyebrowClass}>{eyebrow}</div>
      <h2 id={titleId} className="mt-1 text-base font-semibold text-white lg:text-xl">
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
 * the modal exits: the right edge on wide viewports, the top-right corner on
 * narrow ones (where the dialog slides down rather than sideways).
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

  // Wide viewports: glow biased toward the slide-off edge. Mobile top-corner
  // tab uses a centered pulse so the affordance reads symmetric on a square btn.
  const restingShadow =
    direction === "right"
      ? "0 18px 36px rgba(0,0,0,0.55), 14px 0 0 -8px rgba(34,211,238,0)"
      : "0 8px 20px rgba(0,0,0,0.45), 0 0 0 0 rgba(34,211,238,0)";
  const pulseShadow =
    direction === "right"
      ? "0 18px 36px rgba(0,0,0,0.55), 18px 0 36px rgba(34,211,238,0.65)"
      : "0 8px 20px rgba(0,0,0,0.45), 0 0 24px rgba(34,211,238,0.65)";

  const positionClass =
    direction === "right"
      ? "right-0 top-1/2 h-24 w-9 -translate-y-1/2 translate-x-1/3"
      : "right-3 top-1 h-9 w-9 -translate-y-1/3";

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
 * X close affordance in the dialog's top-right corner. Sits inside the card,
 * over the pinned header, so it stays in view while the body scrolls; z-20
 * rides above the gradient overlay but below the peek tab. Renders only when the
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
