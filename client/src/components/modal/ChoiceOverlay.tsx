import { useCallback, useEffect, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { menuButtonClass } from "../menu/buttonStyles.ts";
import { useHorizontalScroll } from "../../hooks/useHorizontalScroll.ts";
import { useOptionalDialogPeek } from "./dialogPeekContext.ts";
import { PeekButton } from "./DialogShell.tsx";

export function ChoiceOverlay({
  title,
  subtitle,
  children,
  footer,
  widthClassName = "w-full",
  maxWidthClassName = "max-w-6xl",
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
  footer?: React.ReactNode;
  widthClassName?: string;
  maxWidthClassName?: string;
}) {
  const { t } = useTranslation("game");
  const contentRef = useRef<HTMLDivElement>(null);
  const peek = useOptionalDialogPeek();
  const [canScrollUp, setCanScrollUp] = useState(false);
  const [canScrollDown, setCanScrollDown] = useState(false);

  // Overlay scrollbars (macOS, touch) are invisible until the user scrolls, so
  // a tall option list reads as clipped. Edge fades make the overflow explicit.
  const updateScrollState = useCallback(() => {
    const el = contentRef.current;
    if (!el) return;
    setCanScrollUp(el.scrollTop > 1);
    setCanScrollDown(el.scrollTop < el.scrollHeight - el.clientHeight - 1);
  }, []);

  useEffect(() => {
    const el = contentRef.current;
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
  }, [updateScrollState, children]);

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

/** Scrollable card strip wrapper with edge arrow buttons, mousewheel, and drag support. */
export function ScrollableCardStrip({
  children,
  className = "",
  stripClassName = "card-choice-strip",
  innerClassName = "mx-auto flex min-h-0 flex-1 items-center gap-2 px-1 pt-2 pb-2 lg:gap-3",
}: {
  children: React.ReactNode;
  className?: string;
  stripClassName?: string;
  innerClassName?: string;
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
    <div className={`relative ${className}`}>
      <div
        ref={stripRef}
        className={`${stripClassName} ${innerClassName}`}
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
