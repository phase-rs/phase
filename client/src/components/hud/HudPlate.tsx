import type { ReactNode } from "react";
import { motion, useReducedMotion } from "framer-motion";

import { UnderAttackOverlay } from "./UnderAttackOverlay.tsx";

type HudTone = "neutral" | "emerald" | "rose" | "cyan" | "amber";

interface HudPlateProps {
  label: string;
  tone?: HudTone;
  onClick?: () => void;
  children: ReactNode;
  trailing?: ReactNode;
  /** When true, apply the active-turn treatment: heavy tinted ring plus a
   *  pulsing glow (suppressed under prefers-reduced-motion, but the heavy
   *  ring always applies so the signal is still legible). */
  active?: boolean;
  /** Per-seat identity color. Rendered as a small dot adjacent to the label
   *  — orthogonal to `tone` (which encodes game-state: turn, target). */
  seatColor?: string;
  /** Passive imposed state: one or more creatures are attacking this player.
   *  Renders a red ring + pulse overlay layered atop the tone treatment, so
   *  "it's my turn AND I'm under attack" stays legible. Motion suppressed
   *  under prefers-reduced-motion. */
  underAttack?: boolean;
}

const TONE_CLASSES: Record<HudTone, string> = {
  neutral: "border-white/12 bg-slate-950/72 text-slate-100 shadow-[0_16px_48px_rgba(15,23,42,0.45)]",
  emerald: "border-emerald-400/30 bg-emerald-950/40 text-emerald-50 shadow-[0_16px_48px_rgba(16,185,129,0.18)]",
  rose: "border-rose-400/30 bg-rose-950/38 text-rose-50 shadow-[0_16px_48px_rgba(244,63,94,0.18)]",
  cyan: "border-cyan-400/40 bg-cyan-950/42 text-cyan-50 shadow-[0_16px_48px_rgba(34,211,238,0.2)]",
  amber: "border-amber-400/30 bg-amber-950/38 text-amber-50 shadow-[0_16px_48px_rgba(245,158,11,0.18)]",
};

/** Active-turn rings — heavier than the default tone border. Drives both
 *  the static outline and the pulse color. Kept in one place so the ring
 *  and the animated box-shadow stay chromatically in sync. */
const ACTIVE_RING_CLASSES: Record<HudTone, string> = {
  neutral: "ring-2 ring-white/45",
  emerald: "ring-2 ring-emerald-300/70",
  rose: "ring-2 ring-rose-300/70",
  cyan: "ring-2 ring-cyan-300/70",
  amber: "ring-2 ring-amber-300/70",
};

const ACTIVE_PULSE_RGBA: Record<HudTone, [string, string]> = {
  neutral: ["rgba(255, 255, 255, 0.35)", "rgba(255, 255, 255, 0.6)"],
  emerald: ["rgba(52, 211, 153, 0.35)", "rgba(52, 211, 153, 0.65)"],
  rose: ["rgba(251, 113, 133, 0.35)", "rgba(251, 113, 133, 0.65)"],
  cyan: ["rgba(34, 211, 238, 0.35)", "rgba(34, 211, 238, 0.65)"],
  amber: ["rgba(251, 191, 36, 0.35)", "rgba(251, 191, 36, 0.65)"],
};

export function HudPlate({
  label,
  tone = "neutral",
  onClick,
  children,
  trailing,
  active = false,
  seatColor,
  underAttack = false,
}: HudPlateProps) {
  const Component = onClick ? "button" : "div";
  const shouldReduceMotion = useReducedMotion();
  const activeRing = active ? ` ${ACTIVE_RING_CLASSES[tone]} ring-offset-2 ring-offset-black/40` : "";
  const [pulseLo, pulseHi] = ACTIVE_PULSE_RGBA[tone];

  return (
    <Component
      type={onClick ? "button" : undefined}
      onClick={onClick}
      className={`group relative inline-flex max-w-full items-center gap-2 rounded-[18px] border px-2.5 py-1.5 backdrop-blur-xl transition-all duration-200 ${TONE_CLASSES[tone]}${activeRing} ${
        onClick ? "cursor-pointer hover:-translate-y-0.5 hover:border-white/30" : ""
      }`}
    >
      {active && !shouldReduceMotion && (
        <motion.div
          aria-hidden
          className="pointer-events-none absolute -inset-0.5 rounded-[20px]"
          animate={{
            boxShadow: [
              `0 0 0 0 ${pulseLo}, 0 0 14px 2px ${pulseLo}`,
              `0 0 0 2px ${pulseHi}, 0 0 26px 6px ${pulseHi}`,
            ],
          }}
          transition={{
            duration: 1.2,
            repeat: Infinity,
            repeatType: "reverse",
            ease: "easeInOut",
          }}
        />
      )}
      {/* Under-attack overlay — layered atop the active-turn pulse so "my
          turn + I'm being attacked" renders both signals. */}
      {underAttack && (
        <>
          <UnderAttackOverlay />
          <span className="sr-only">{label} is under attack</span>
        </>
      )}
      <div className="absolute inset-[1px] rounded-[16px] bg-gradient-to-b from-white/8 via-transparent to-black/10" />
      <div className="relative min-w-0">
        <div className="mb-0.5 flex items-center justify-center gap-1.5">
          {seatColor && (
            <span
              aria-hidden
              className="h-1.5 w-1.5 shrink-0 rounded-full ring-1 ring-black/30"
              style={{ backgroundColor: seatColor }}
            />
          )}
          <span className="text-[9px] font-semibold uppercase tracking-[0.18em] text-white/68">
            {label}
          </span>
        </div>
        <div className="flex items-center justify-center gap-2">
          {children}
        </div>
      </div>
      {trailing ? (
        <div className="relative flex shrink-0 items-center gap-1.5">
          {trailing}
        </div>
      ) : null}
    </Component>
  );
}
