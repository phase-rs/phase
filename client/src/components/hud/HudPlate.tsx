import type { CSSProperties, ReactNode } from "react";
import { motion, useReducedMotion } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { PlayerId } from "../../adapter/types.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { AvatarHoverPreview } from "./AvatarHoverPreview.tsx";
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
  /** Planeswalker art crop URL for the player avatar. */
  avatarUrl?: string | null;
  /** When set, the plate renders a fuchsia debug-highlight ring iff this
   *  player matches `useUiStore.debugHighlightedPlayerId`. Threaded through
   *  by both `PlayerHud` and `OpponentHud`; absence means the plate never
   *  participates in debug highlighting. */
  playerId?: PlayerId;
  density?: "default" | "compact";
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
  avatarUrl,
  playerId,
  density = "default",
}: HudPlateProps) {
  const { t } = useTranslation("game");
  const Component = onClick ? "button" : "div";
  const shouldReduceMotion = useReducedMotion();
  const activeRing = active ? ` ${ACTIVE_RING_CLASSES[tone]} ring-offset-2 ring-offset-black/40` : "";
  const [pulseLo, pulseHi] = ACTIVE_PULSE_RGBA[tone];
  const isDebugHighlighted = useUiStore(
    (s) => playerId != null && s.debugHighlightedPlayerId === playerId,
  );
  const compact = density === "compact";
  const plateChrome = compact
    ? "gap-1 rounded-lg px-1 py-0.5"
    : "gap-2 rounded-xl px-1.5 py-1 lg:gap-2.5 lg:rounded-[18px] lg:px-2.5 lg:py-1.5";
  const labelClass = compact
    ? "truncate text-[8px] font-semibold uppercase tracking-[0.12em]"
    : "truncate text-[9px] font-semibold uppercase tracking-[0.18em]";
  const contentGap = compact ? "gap-0.5" : "gap-1";
  const childGap = compact ? "gap-1" : "gap-2";
  const trailingClass = compact
    ? "relative flex max-w-[36vw] shrink items-center gap-0.5 overflow-hidden [&>*]:scale-90 [&>*]:origin-center"
    : "relative flex shrink-0 items-center gap-1.5";

  const plate = (
    <Component
      type={onClick ? "button" : undefined}
      onClick={onClick}
      className={`group relative inline-flex max-w-full items-center border backdrop-blur-xl transition-all duration-200 ${plateChrome} ${TONE_CLASSES[tone]}${activeRing} ${
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
      {underAttack && (
        <>
          <UnderAttackOverlay />
          <span className="sr-only">{t("avatar.underAttack", { name: label })}</span>
        </>
      )}
      {isDebugHighlighted && (
        <div
          aria-hidden
          className="pointer-events-none absolute -inset-1 z-30 rounded-2xl ring-4 ring-fuchsia-400 shadow-[0_0_22px_6px_rgba(232,121,249,0.7),inset_0_0_18px_4px_rgba(232,121,249,0.45)] animate-pulse"
        />
      )}
      <div className="absolute inset-[1px] rounded-[16px] bg-gradient-to-b from-white/8 via-transparent to-black/10" />
      {avatarUrl ? (
        <HudAvatar
          label={label}
          avatarUrl={avatarUrl}
          seatColor={seatColor}
          compact={compact}
        />
      ) : null}
      <div className={`relative flex min-w-0 flex-col items-center justify-center ${contentGap}`}>
        <div className={`flex min-w-0 items-center justify-center ${contentGap}`}>
          {!avatarUrl && seatColor && (
            <span
              aria-hidden
              className={`${compact ? "h-2 w-2" : "h-2.5 w-2.5"} shrink-0 rounded-full ring-1 ring-black/30 shadow-[0_0_6px_var(--seat-glow)]`}
              style={{ backgroundColor: seatColor, "--seat-glow": `${seatColor}88` } as CSSProperties}
            />
          )}
          <span
            className={labelClass}
            style={seatColor ? { color: seatColor } : { color: "rgba(255,255,255,0.68)" }}
          >
            {label}
          </span>
        </div>
        <div className={`flex min-w-0 items-center justify-center ${childGap}`}>
          {children}
        </div>
      </div>
      {trailing ? (
        <div className={trailingClass}>
          {trailing}
        </div>
      ) : null}
    </Component>
  );

  return plate;
}

function HudAvatar({
  label,
  avatarUrl,
  seatColor,
  compact,
}: {
  label: string;
  avatarUrl: string;
  seatColor?: string;
  compact: boolean;
}) {
  return (
    <AvatarHoverPreview
      avatarUrl={avatarUrl}
      label={label}
      seatColor={seatColor}
      className={`relative shrink-0 overflow-hidden rounded-lg border border-white/15 bg-slate-950 shadow-[0_10px_24px_rgba(0,0,0,0.35)] ${compact ? "h-8 w-7" : "h-12 w-10 lg:h-14 lg:w-12"}`}
      style={seatColor ? {
        borderColor: `${seatColor}cc`,
        boxShadow: `0 0 0 1px ${seatColor}55, 0 10px 24px rgba(0,0,0,0.35), 0 0 18px ${seatColor}33`,
      } : undefined}
    >
      <img
        src={avatarUrl}
        alt={label}
        className="h-full w-full object-cover"
      />
      <div className="absolute inset-0 bg-gradient-to-b from-white/12 via-transparent to-black/32" />
    </AvatarHoverPreview>
  );
}
