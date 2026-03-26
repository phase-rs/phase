import { useCallback, useEffect, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";

import { menuButtonClass } from "../menu/buttonStyles.ts";

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
  const contentRef = useRef<HTMLDivElement>(null);

  // Translate vertical mousewheel into horizontal scroll on card strips
  useEffect(() => {
    const container = contentRef.current;
    if (!container) return;

    const handleWheel = (e: WheelEvent) => {
      const strip = (e.target as HTMLElement).closest<HTMLElement>(
        ".card-choice-strip",
      );
      if (!strip) return;
      if (strip.scrollWidth <= strip.clientWidth) return;
      e.preventDefault();
      strip.scrollLeft += e.deltaY || e.deltaX;
    };

    container.addEventListener("wheel", handleWheel, { passive: false });
    return () => container.removeEventListener("wheel", handleWheel);
  }, []);

  return (
    <div className="fixed inset-0 z-50 flex flex-col px-0 py-0 lg:items-center lg:justify-center lg:px-4 lg:py-6">
      <div className="absolute inset-0 bg-[radial-gradient(circle_at_top,rgba(31,41,55,0.55),rgba(2,6,23,0.92)_58%,rgba(2,6,23,0.98))]" />
      <motion.div
        className={`card-scale-reset relative flex h-full flex-col overflow-hidden border-white/10 bg-[#0b1020]/94 shadow-[0_32px_90px_rgba(0,0,0,0.48)] backdrop-blur-md lg:h-auto lg:max-h-[calc(100vh_-_3rem)] lg:rounded-[28px] lg:border ${widthClassName} ${maxWidthClassName}`}
        initial={{ opacity: 0, y: 18, scale: 0.98 }}
        animate={{ opacity: 1, y: 0, scale: 1 }}
        transition={{ duration: 0.24, ease: "easeOut" }}
      >
        <div className="modal-header-compact relative shrink-0 border-b border-white/10">
          <div className="modal-eyebrow uppercase tracking-[0.24em] text-slate-500 lg:absolute lg:right-4 lg:top-3">
            Game Choice
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
        <div ref={contentRef} className="flex min-h-0 flex-1 flex-col overflow-y-auto px-2 pt-3 pb-2 lg:px-5 lg:pt-5 lg:pb-5">
          {children}
        </div>
        {footer && (
          <div className="shrink-0 border-t border-white/5 px-2 pb-3 pt-1 lg:px-5 lg:pb-5 lg:pt-2">
            {footer}
          </div>
        )}
      </motion.div>
    </div>
  );
}

/** Scrollable card strip wrapper with edge arrow buttons and mousewheel support. */
export function ScrollableCardStrip({
  children,
  className = "",
}: {
  children: React.ReactNode;
  className?: string;
}) {
  const stripRef = useRef<HTMLDivElement>(null);
  const [canScrollLeft, setCanScrollLeft] = useState(false);
  const [canScrollRight, setCanScrollRight] = useState(false);

  const updateScrollState = useCallback(() => {
    const el = stripRef.current;
    if (!el) return;
    setCanScrollLeft(el.scrollLeft > 1);
    setCanScrollRight(el.scrollLeft < el.scrollWidth - el.clientWidth - 1);
  }, []);

  useEffect(() => {
    const el = stripRef.current;
    if (!el) return;

    // Check after children render / images load
    updateScrollState();
    const observer = new ResizeObserver(updateScrollState);
    observer.observe(el);
    el.addEventListener("scroll", updateScrollState, { passive: true });

    return () => {
      observer.disconnect();
      el.removeEventListener("scroll", updateScrollState);
    };
  }, [updateScrollState]);

  const scroll = useCallback((direction: -1 | 1) => {
    const el = stripRef.current;
    if (!el) return;
    el.scrollBy({ left: direction * el.clientWidth * 0.6, behavior: "smooth" });
  }, []);

  return (
    <div className="relative">
      <div
        ref={stripRef}
        className={`card-choice-strip mx-auto flex min-h-0 flex-1 items-center gap-2 px-1 pb-2 lg:gap-3 ${className}`}
      >
        {children}
      </div>

      {/* Left scroll button — gradient is pointer-events-none so cards beneath remain clickable */}
      <AnimatePresence>
        {canScrollLeft && (
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.15 }}
            className="pointer-events-none absolute left-0 top-0 z-10 flex h-[calc(100%-8px)] w-10 items-center justify-center bg-gradient-to-r from-black/60 to-transparent lg:w-12"
          >
            <button
              onClick={() => scroll(-1)}
              className="pointer-events-auto rounded-full bg-black/40 p-1 backdrop-blur-sm transition hover:bg-black/60"
              aria-label="Scroll left"
            >
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-5 w-5 text-white/90">
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
            className="pointer-events-none absolute right-0 top-0 z-10 flex h-[calc(100%-8px)] w-10 items-center justify-center bg-gradient-to-l from-black/60 to-transparent lg:w-12"
          >
            <button
              onClick={() => scroll(1)}
              className="pointer-events-auto rounded-full bg-black/40 p-1 backdrop-blur-sm transition hover:bg-black/60"
              aria-label="Scroll right"
            >
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-5 w-5 text-white/90">
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
  label = "Confirm",
}: {
  onClick: () => void;
  disabled?: boolean;
  label?: string;
}) {
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
          {label}
        </button>
      </motion.div>
    </AnimatePresence>
  );
}
