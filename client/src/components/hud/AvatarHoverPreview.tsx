import { type CSSProperties, type ReactNode, useCallback, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, motion, useReducedMotion } from "framer-motion";

interface Anchor {
  left: number;
  top: number;
  placement: "above" | "below";
}

interface Props {
  avatarUrl: string;
  label: string;
  children: ReactNode;
  className?: string;
  style?: CSSProperties;
  title?: string;
  /** Per-seat identity color, used to tint the preview's outer glow and
   *  accent line so the hover preview is visually tied to the player it
   *  represents. Falls back to a neutral platinum when absent. */
  seatColor?: string;
}

const DEFAULT_ACCENT = "rgba(226, 232, 240, 0.9)";

export function AvatarHoverPreview({
  avatarUrl,
  label,
  children,
  className,
  style,
  title,
  seatColor,
}: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const shouldReduceMotion = useReducedMotion();

  const show = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const placement: Anchor["placement"] =
      rect.top < window.innerHeight / 2 ? "below" : "above";
    setAnchor({
      left: rect.left + rect.width / 2,
      top: placement === "below" ? rect.bottom + 12 : rect.top - 12,
      placement,
    });
  }, []);

  const hide = useCallback(() => setAnchor(null), []);

  const accent = seatColor ?? DEFAULT_ACCENT;

  return (
    <div
      ref={ref}
      className={className}
      style={style}
      title={title}
      onMouseEnter={show}
      onMouseLeave={hide}
      onFocus={show}
      onBlur={hide}
    >
      {children}
      {createPortal(
        <AnimatePresence>
          {anchor && (
            <motion.div
              key="avatar-preview"
              className="pointer-events-none fixed z-50"
              style={{
                left: anchor.left,
                top: anchor.top,
                transformOrigin:
                  anchor.placement === "above" ? "50% 100%" : "50% 0%",
              }}
              initial={{
                opacity: 0,
                scale: shouldReduceMotion ? 1 : 0.94,
                y: shouldReduceMotion
                  ? 0
                  : anchor.placement === "above"
                    ? 6
                    : -6,
                x: "-50%",
                ...(anchor.placement === "above" ? { translateY: "-100%" } : {}),
              }}
              animate={{
                opacity: 1,
                scale: 1,
                y: 0,
                x: "-50%",
                ...(anchor.placement === "above" ? { translateY: "-100%" } : {}),
              }}
              exit={{
                opacity: 0,
                scale: shouldReduceMotion ? 1 : 0.96,
                x: "-50%",
                ...(anchor.placement === "above" ? { translateY: "-100%" } : {}),
              }}
              transition={{ duration: 0.18, ease: [0.22, 0.61, 0.36, 1] }}
            >
              <PreviewCard avatarUrl={avatarUrl} label={label} accent={accent} />
            </motion.div>
          )}
        </AnimatePresence>,
        document.body,
      )}
    </div>
  );
}

function PreviewCard({
  avatarUrl,
  label,
  accent,
}: {
  avatarUrl: string;
  label: string;
  accent: string;
}) {
  return (
    <div
      className="relative rounded-2xl p-[1.5px]"
      style={{
        background: `linear-gradient(140deg, ${accent}cc 0%, rgba(15,23,42,0.6) 40%, ${accent}66 100%)`,
        boxShadow:
          `0 30px 70px rgba(0,0,0,0.65),`
          + ` 0 0 0 1px rgba(255,255,255,0.05),`
          + ` 0 0 28px ${accent}33`,
      }}
    >
      <div className="relative overflow-hidden rounded-[14px] border border-white/10 bg-slate-950">
        <img
          src={avatarUrl}
          alt={label}
          className="block h-auto w-72 max-w-[60vw] object-cover"
          draggable={false}
        />
        {/* Top sheen + bottom vignette for portrait feel */}
        <div className="pointer-events-none absolute inset-0 bg-gradient-to-b from-white/10 via-transparent to-black/55" />
        {/* Inner hairline frame */}
        <div className="pointer-events-none absolute inset-[3px] rounded-[10px] ring-1 ring-white/10" />
        {/* Accent corner glints */}
        <div
          className="pointer-events-none absolute -inset-[1px] rounded-[14px]"
          style={{
            background:
              `radial-gradient(120% 60% at 0% 0%, ${accent}26, transparent 55%),`
              + ` radial-gradient(120% 60% at 100% 100%, ${accent}1f, transparent 55%)`,
          }}
        />
        {/* Name plate */}
        <div className="absolute inset-x-0 bottom-0 px-3 pb-2.5 pt-6">
          <div
            className="mb-1 h-px w-full"
            style={{
              background:
                `linear-gradient(90deg, transparent, ${accent}b3, transparent)`,
            }}
          />
          <div
            className="truncate text-center text-[11px] font-semibold uppercase tracking-[0.28em] text-white"
            style={{
              textShadow: "0 1px 2px rgba(0,0,0,0.85), 0 0 12px rgba(0,0,0,0.6)",
            }}
          >
            {label}
          </div>
        </div>
      </div>
    </div>
  );
}
