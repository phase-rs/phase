import type { CSSProperties, ReactNode } from "react";
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
  cornerBadge?: ReactNode;
  /** When true, apply the active-turn treatment. */
  active?: boolean;
  /** Per-seat identity color. Rendered as a small dot adjacent to the label
   *  — orthogonal to `tone` (which encodes game-state: turn, target). */
  seatColor?: string;
  /** Passive imposed state: one or more creatures are attacking this player. */
  underAttack?: boolean;
  /** Planeswalker art crop URL for the player avatar. */
  avatarUrl?: string | null;
  /** When set, the plate renders a fuchsia debug-highlight ring iff this
   *  player matches `useUiStore.debugHighlightedPlayerId`. Threaded through
   *  by both `PlayerHud` and `OpponentHud`; absence means the plate never
   *  participates in debug highlighting. */
  playerId?: PlayerId;
  /** Decision authority (orthogonal to `active`/turn): this seat is the one the
   *  game is currently waiting on to make a choice. Decorative — narration lives
   *  in `TurnStatusLine`. */
  hasPendingDecision?: boolean;
  density?: "default" | "compact";
}

const TONE_CLASSES: Record<HudTone, string> = {
  neutral: "border-white/12 bg-slate-950/90 text-slate-100 shadow-[0_1px_0_rgba(255,255,255,0.05)]",
  emerald: "border-emerald-400/28 bg-emerald-950/72 text-emerald-50 shadow-[0_1px_0_rgba(255,255,255,0.05)]",
  rose: "border-rose-400/28 bg-rose-950/72 text-rose-50 shadow-[0_1px_0_rgba(255,255,255,0.05)]",
  cyan: "border-cyan-400/32 bg-cyan-950/72 text-cyan-50 shadow-[0_1px_0_rgba(255,255,255,0.05)]",
  amber: "border-amber-400/28 bg-amber-950/72 text-amber-50 shadow-[0_1px_0_rgba(255,255,255,0.05)]",
};

const ACTIVE_RING_CLASSES: Record<HudTone, string> = {
  neutral: "ring-1 ring-white/45 border-white/28",
  emerald: "ring-1 ring-emerald-300/65 border-emerald-300/55",
  rose: "ring-1 ring-rose-300/65 border-rose-300/55",
  cyan: "ring-1 ring-cyan-300/65 border-cyan-300/55",
  amber: "ring-1 ring-amber-300/65 border-amber-300/55",
};

export function HudPlate({
  label,
  tone = "neutral",
  onClick,
  children,
  trailing,
  cornerBadge,
  active = false,
  seatColor,
  underAttack = false,
  avatarUrl,
  playerId,
  hasPendingDecision = false,
  density = "default",
}: HudPlateProps) {
  const { t } = useTranslation("game");
  const Component = onClick ? "button" : "div";
  const activeRing = active ? ` ${ACTIVE_RING_CLASSES[tone]}` : "";
  const isDebugHighlighted = useUiStore(
    (s) => playerId != null && s.debugHighlightedPlayerId === playerId,
  );
  const compact = density === "compact";
  const plateChrome = compact
    ? "gap-1 rounded-lg px-1 py-0.5"
    : "gap-2 rounded-[10px] px-1.5 py-1 lg:gap-2.5 lg:px-2.5 lg:py-1.5";
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
      data-hud-plate=""
      className={`group relative inline-flex max-w-full items-center border transition-colors duration-150 ${plateChrome} ${TONE_CLASSES[tone]}${activeRing} ${
        onClick ? "cursor-pointer hover:border-white/30 hover:bg-slate-900/92" : ""
      }`}
    >
      {underAttack && (
        <>
          <UnderAttackOverlay />
          <span className="sr-only">{t("avatar.underAttack", { name: label })}</span>
        </>
      )}
      {isDebugHighlighted && (
        <div
          aria-hidden
          className="pointer-events-none absolute inset-0 z-30 rounded-[10px] outline-2 outline-fuchsia-300"
        />
      )}
      {hasPendingDecision && (
        <span
          aria-hidden
          className="pointer-events-none absolute left-1 top-1 z-30 h-1.5 w-5 rounded-[2px] bg-amber-300"
        />
      )}
      {cornerBadge ? (
        <div className="absolute -top-0.5 left-1/2 z-40 -translate-x-1/2 -translate-y-1/2">{cornerBadge}</div>
      ) : null}
      <div className="absolute inset-[1px] rounded-[9px] border-t border-white/8" />
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
              className={`${compact ? "h-2 w-2" : "h-2.5 w-2.5"} shrink-0 rounded-full ring-1 ring-black/30`}
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
        <div className={trailingClass} data-hud-plate-trailing="">
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
      title={label}
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
