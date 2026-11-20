import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import type { ReactElement, RefObject } from "react";
import { AnimatePresence, motion } from "framer-motion";
import type { MotionProps } from "framer-motion";
import { useTranslation } from "react-i18next";

import { menuButtonClass } from "../menu/buttonStyles.ts";
import { useChoiceCardLayout } from "../../hooks/useChoiceCardLayout.ts";
import { useHorizontalScroll } from "../../hooks/useHorizontalScroll.ts";
import { useOptionalDialogPeek } from "./dialogPeekContext.ts";
import { PeekButton } from "./DialogShell.tsx";
import type { ObjectId } from "../../adapter/types.ts";

type ScrollEdges = {
  canScrollUp: boolean;
  canScrollDown: boolean;
};

/**
 * Whether `scrollerRef`'s element can scroll further up or down. Overlay
 * scrollbars (macOS, touch) are invisible until the user scrolls, so a clipped
 * list reads as complete; ScrollEdgeFades makes the overflow explicit.
 * `content` re-subscribes when what the scroller holds changes.
 */
function useScrollEdges(scrollerRef: RefObject<HTMLElement | null>, content: unknown): ScrollEdges {
  const [canScrollUp, setCanScrollUp] = useState(false);
  const [canScrollDown, setCanScrollDown] = useState(false);

  const updateScrollState = useCallback(() => {
    const el = scrollerRef.current;
    if (!el) return;
    setCanScrollUp(el.scrollTop > 1);
    setCanScrollDown(el.scrollTop < el.scrollHeight - el.clientHeight - 1);
  }, [scrollerRef]);

  useEffect(() => {
    const el = scrollerRef.current;
    if (!el) return;
    updateScrollState();
    const observer = new ResizeObserver(updateScrollState);
    observer.observe(el);
    for (const child of Array.from(el.children)) observer.observe(child);
    el.addEventListener("scroll", updateScrollState, { passive: true });
    return () => {
      observer.disconnect();
      el.removeEventListener("scroll", updateScrollState);
    };
  }, [scrollerRef, updateScrollState, content]);

  return { canScrollUp, canScrollDown };
}

/**
 * Fades over a scroller's top and bottom edges while it can scroll that way,
 * the bottom one with a chevron. Place it after the scroller, inside a
 * `relative` wrapper the scroller fills.
 */
function ScrollEdgeFades({ canScrollUp, canScrollDown }: ScrollEdges) {
  return (
    <>
      <div
        aria-hidden
        className={`pointer-events-none absolute inset-x-0 top-0 h-8 bg-gradient-to-b from-[#0b1020] to-transparent transition-opacity duration-150 ${
          canScrollUp ? "opacity-100" : "opacity-0"
        }`}
      />
      <div
        aria-hidden
        className={`pointer-events-none absolute inset-x-0 bottom-0 flex h-12 items-end justify-center bg-gradient-to-t from-[#0b1020] to-transparent pb-1 transition-opacity duration-150 ${
          canScrollDown ? "opacity-100" : "opacity-0"
        }`}
      >
        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-5 w-5 text-white/70">
          <path fillRule="evenodd" d="M5.22 8.22a.75.75 0 0 1 1.06 0L10 11.94l3.72-3.72a.75.75 0 1 1 1.06 1.06l-4.25 4.25a.75.75 0 0 1-1.06 0L5.22 9.28a.75.75 0 0 1 0-1.06Z" clipRule="evenodd" />
        </svg>
      </div>
    </>
  );
}

type ChoiceOverlayProps = {
  title: string;
  children: React.ReactNode;
  widthClassName?: string;
  maxWidthClassName?: string;
} & (
  | {
      subtitle: string;
      footer?: React.ReactNode;
      headerActions?: never;
    }
  | {
      /**
       * Opt-in single-line header, for a short screen where every line of
       * chrome takes height from the cards. The title is the header's only
       * text and these actions sit at the end of its line, so the prompt
       * passes its instruction as the title, after any context its usual
       * title adds, and its confirm (a ChoiceHeaderConfirm) here. The
       * eyebrow, subtitle and footer are left out: the title already says
       * what to do, and the confirm has moved up beside it.
       */
      headerActions: React.ReactElement;
      subtitle?: never;
      footer?: never;
    }
);

export function ChoiceOverlay({
  title,
  subtitle,
  headerActions,
  children,
  footer,
  widthClassName = "w-full",
  maxWidthClassName = "max-w-6xl",
}: ChoiceOverlayProps) {
  const { t } = useTranslation("game");
  const contentRef = useRef<HTMLDivElement>(null);
  const peek = useOptionalDialogPeek();
  const scrollEdges = useScrollEdges(contentRef, children);

  return (
    <div
      className="fixed inset-0 z-50 flex flex-col px-0 py-0 lg:items-center lg:justify-center lg:px-4 lg:py-6"
      data-card-preview-dock="side"
    >
      <div className="absolute inset-0 bg-black/68" />
      <div className={`relative flex h-full flex-col lg:h-auto lg:max-h-[calc(100vh_-_3rem)] ${widthClassName} ${maxWidthClassName}`}>
        <motion.div
          className="card-scale-reset relative flex h-full min-h-0 flex-col overflow-hidden border-white/10 bg-[#0b1020] shadow-[0_18px_48px_rgba(0,0,0,0.48)] lg:h-auto lg:rounded-[12px] lg:border"
          initial={{ opacity: 0, y: 18, scale: 0.98 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          transition={{ duration: 0.24, ease: "easeOut" }}
        >
          <div className="modal-header-compact relative shrink-0 border-b border-white/10">
            {headerActions !== undefined ? (
              // The actions sit in the screen's top corner, which a landscape
              // phone's rounded corner cuts into under `viewport-fit=cover`, so
              // the line keeps clear of the safe-area insets (0 elsewhere).
              <div className="flex items-center gap-3 pl-[env(safe-area-inset-left)] pr-[env(safe-area-inset-right)]">
                <h2 className="min-w-0 flex-1 font-semibold text-white">
                  {title}
                </h2>
                <div className="flex shrink-0 items-center gap-2">
                  {headerActions}
                </div>
              </div>
            ) : (
              <>
                <div className="modal-eyebrow uppercase tracking-[0.24em] text-slate-500 lg:absolute lg:right-4 lg:top-3">
                  {t("choiceOverlay.eyebrow")}
                </div>
                <div className="lg:flex lg:items-baseline lg:gap-3">
                  <h2 className="shrink-0 font-semibold text-white">
                    {title}
                  </h2>
                  <p className="modal-subtitle text-slate-400 lg:mt-0">
                    {subtitle}
                  </p>
                </div>
              </>
            )}
          </div>
          {/* When the peek tab is mounted it overlaps the card's right edge (~24px),
              so reserve right clearance to keep edge-aligned interactive content
              (e.g. the rightmost color symbol) selectable instead of under the tab. */}
          <div className="relative flex min-h-0 flex-1 flex-col">
            <div
              ref={contentRef}
              className={`thin-scrollbar flex min-h-0 flex-1 flex-col overflow-y-auto pl-2 pt-3 pb-2 lg:pl-5 lg:pt-5 lg:pb-5 ${
                peek ? "pr-10 lg:pr-12" : "pr-2 lg:pr-5"
              }`}
            >
              {children}
            </div>
            <ScrollEdgeFades {...scrollEdges} />
          </div>
          {footer && (
            <div className="shrink-0 border-t border-white/5 px-2 pb-3 pt-1 lg:px-5 lg:pb-5 lg:pt-2">
              {footer}
            </div>
          )}
        </motion.div>
        {peek ? <PeekButton onClick={peek.togglePeek} /> : null}
      </div>
    </div>
  );
}

/**
 * Scrollable card strip wrapper with edge arrow buttons, mousewheel, and drag support.
 *
 * Below lg the frame sizes the cards from the room it has (`.card-strip-frame`
 * in index.css): it grows to fill its parent, so place the strip in a flex
 * column with a definite height, such as an overlay body. There the frame is a
 * size container and so has no width of its own: the parent must stretch it
 * across its width, as a flex column does by default. From lg up the cards
 * keep the board size.
 */
export function ScrollableCardStrip({
  children,
  stripClassName = "card-choice-strip",
}: {
  children: React.ReactNode;
  stripClassName?: string;
}) {
  const { t } = useTranslation("game");
  const stripRef = useHorizontalScroll<HTMLDivElement>();
  const [canScrollLeft, setCanScrollLeft] = useState(false);
  const [canScrollRight, setCanScrollRight] = useState(false);

  const updateScrollState = useCallback(() => {
    const el = stripRef.current;
    if (!el) return;
    setCanScrollLeft(el.scrollLeft > 1);
    setCanScrollRight(el.scrollLeft < el.scrollWidth - el.clientWidth - 1);
  }, [stripRef]);

  useEffect(() => {
    const el = stripRef.current;
    if (!el) return;

    updateScrollState();
    const observer = new ResizeObserver(updateScrollState);
    observer.observe(el);
    el.addEventListener("scroll", updateScrollState, { passive: true });

    return () => {
      observer.disconnect();
      el.removeEventListener("scroll", updateScrollState);
    };
  }, [updateScrollState, stripRef]);

  const scroll = useCallback(
    (direction: -1 | 1) => {
      const el = stripRef.current;
      if (!el) return;
      el.scrollBy({ left: direction * el.clientWidth * 0.6, behavior: "smooth" });
    },
    [stripRef],
  );

  return (
    <div className="card-strip-frame relative">
      <div
        ref={stripRef}
        className={`${stripClassName} flex min-h-0 flex-1 items-center gap-2 px-1 pt-2 pb-2 lg:gap-3`}
      >
        {children}
      </div>

      {/* Left scroll affordance is pointer-events-none so cards beneath remain clickable. */}
      <AnimatePresence>
        {canScrollLeft && (
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.15 }}
            className="pointer-events-none absolute left-0 top-0 z-10 flex h-[calc(100%-8px)] w-14 items-center justify-center bg-black/40 lg:w-16"
          >
            <button
              onClick={() => scroll(-1)}
              className="pointer-events-auto rounded-[8px] border border-white/20 bg-slate-950/90 p-2 shadow-lg shadow-black/40 transition hover:bg-slate-900"
              aria-label={t("choiceOverlay.scrollLeft")}
            >
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-6 w-6 text-white">
                <path fillRule="evenodd" d="M11.78 5.22a.75.75 0 0 1 0 1.06L8.06 10l3.72 3.72a.75.75 0 1 1-1.06 1.06l-4.25-4.25a.75.75 0 0 1 0-1.06l4.25-4.25a.75.75 0 0 1 1.06 0Z" clipRule="evenodd" />
              </svg>
            </button>
          </motion.div>
        )}
      </AnimatePresence>

      {/* Right scroll button */}
      <AnimatePresence>
        {canScrollRight && (
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.15 }}
            className="pointer-events-none absolute right-0 top-0 z-10 flex h-[calc(100%-8px)] w-14 items-center justify-center bg-black/40 lg:w-16"
          >
            <button
              onClick={() => scroll(1)}
              className="pointer-events-auto rounded-[8px] border border-white/20 bg-slate-950/90 p-2 shadow-lg shadow-black/40 transition hover:bg-slate-900"
              aria-label={t("choiceOverlay.scrollRight")}
            >
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-6 w-6 text-white">
                <path fillRule="evenodd" d="M8.22 5.22a.75.75 0 0 1 1.06 0l4.25 4.25a.75.75 0 0 1 0 1.06l-4.25 4.25a.75.75 0 1 1-1.06-1.06L11.94 10 8.22 6.28a.75.75 0 0 1 0-1.06Z" clipRule="evenodd" />
              </svg>
            </button>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

/** How one card of a choice animates in: the motion props the prompt spreads on the card. */
export type ChoiceCardEntry = Pick<MotionProps, "initial" | "transition">;

type ChoiceCardsProps = {
  /** The cards, in the order they are drawn. */
  cards: readonly ObjectId[];
  /**
   * Draws one card as a single element, spreading `entry` on its motion
   * element, or returns null for a card with nothing to draw (no object).
   */
  renderCard: (id: ObjectId, entry: ChoiceCardEntry) => ReactElement | null;
};

// The strip's entrance, staggered along it. Its transition also times the
// card's later animate changes, as it always has on desktop.
const STRIP_CARD_ENTRY_INITIAL = { opacity: 0, y: 60, scale: 0.85 };
const STRIP_CARD_DURATION = 0.35;

// The grid's entrance, the discard grid's quick fade: no stagger, so a card
// far down the grid is there when the player scrolls to it, and a selection
// shows at once.
const GRID_CARD_ENTRY: ChoiceCardEntry = {
  initial: { opacity: 0, y: 24, scale: 0.9 },
  transition: { duration: 0.18 },
};

/**
 * A card choice's cards on a phone: below lg the overlay fills the screen,
 * and the cards fill its width in a grid that scrolls down, at a size whose
 * rules text reads (`.card-choice-grid` in index.css). Each card sits in a
 * cell of the grid's own, which sizes it, so no sizing rule reaches into a
 * card's markup. Fades over the grid's edges show that it scrolls.
 *
 * Place it in a flex column with a definite height, such as an overlay body:
 * it fills the room left, and what sits above it (a filter, pile counts)
 * stays in view while the grid scrolls.
 */
export function ChoiceCardGrid({ cards, renderCard }: ChoiceCardsProps) {
  const scrollerRef = useRef<HTMLDivElement>(null);
  const scrollEdges = useScrollEdges(scrollerRef, cards);

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      <div
        ref={scrollerRef}
        className="thin-scrollbar min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-1 pb-1 pt-2"
      >
        <div className="card-choice-grid">
          {cards.map((id) => {
            const card = renderCard(id, GRID_CARD_ENTRY);
            if (!card) return null;
            return (
              <div key={id} className="card-choice-grid-cell">
                {card}
              </div>
            );
          })}
        </div>
      </div>
      <ScrollEdgeFades {...scrollEdges} />
    </div>
  );
}

/**
 * A card choice's cards: from lg up in a ScrollableCardStrip, as on every
 * other strip prompt, and below lg in the phone's ChoiceCardGrid.
 *
 * Each card's `entry` animates it in. The strip staggers its cards the way it
 * always has. Crossing lg swaps one layout for the other and remounts every
 * card; the strip then skips its entrance, which would otherwise leave cards
 * far along it blank for seconds.
 */
export function ChoiceCards({ cards, renderCard }: ChoiceCardsProps) {
  const { isBelowLg, hasSwitched } = useChoiceCardLayout();

  if (isBelowLg) {
    return <ChoiceCardGrid cards={cards} renderCard={renderCard} />;
  }

  return (
    <ScrollableCardStrip>
      {cards.map((id, index) => (
        // A keyed Fragment adds no element, so each card stays the strip's direct child.
        <Fragment key={id}>
          {renderCard(
            id,
            hasSwitched
              ? { initial: false, transition: { duration: STRIP_CARD_DURATION } }
              : {
                  initial: STRIP_CARD_ENTRY_INITIAL,
                  transition: { delay: 0.1 + index * 0.08, duration: STRIP_CARD_DURATION },
                },
          )}
        </Fragment>
      ))}
    </ScrollableCardStrip>
  );
}

export function ConfirmButton({
  onClick,
  disabled = false,
  label,
}: {
  onClick: () => void;
  disabled?: boolean;
  label?: string;
}) {
  const { t } = useTranslation("game");
  const resolvedLabel = label ?? t("choiceOverlay.confirm");
  return (
    <AnimatePresence>
      <motion.div
        className="mx-auto w-full max-w-xs shrink-0"
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
          {resolvedLabel}
        </button>
      </motion.div>
    </AnimatePresence>
  );
}

/**
 * The confirm for ChoiceOverlay's single-line header (`headerActions`). Where
 * ConfirmButton fills the footer's width, this one sits at the end of the
 * title's line, so it stays compact while keeping a 44px touch height. Every
 * prompt that opts into the header shares it, so the header confirm looks the
 * same in each of them.
 */
export function ChoiceHeaderConfirm({
  onClick,
  disabled = false,
  label,
}: {
  onClick: () => void;
  disabled?: boolean;
  label?: string;
}) {
  const { t } = useTranslation("game");
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className={menuButtonClass({ tone: "cyan", size: "sm", disabled })}
    >
      {label ?? t("choiceOverlay.confirm")}
    </button>
  );
}

export function CancelButton({
  onClick,
  label,
}: {
  onClick: () => void;
  label?: string;
}) {
  const { t } = useTranslation("game");
  const resolvedLabel = label ?? t("choiceOverlay.cancel");
  return (
    <button
      onClick={onClick}
      className={menuButtonClass({
        tone: "slate",
        size: "lg",
        className: "w-full",
      })}
    >
      {resolvedLabel}
    </button>
  );
}
