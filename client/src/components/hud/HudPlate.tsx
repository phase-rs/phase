import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";

import type { PlayerId } from "../../adapter/types.ts";
import { usePlayerAvatarImage } from "../../hooks/usePlayerAvatarImage.ts";
import type { PlayerAvatarIdentity } from "../../services/playerAvatars.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { AvatarHoverPreview } from "./AvatarHoverPreview.tsx";
import { UnderAttackOverlay } from "./UnderAttackOverlay.tsx";

type HudTone = "neutral" | "emerald" | "rose" | "cyan" | "amber";

interface HudPlateProps {
  label: string;
  /** Visually omit the identity row while retaining `label` for avatar and
   *  target-button accessibility. */
  hideLabel?: boolean;
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
  /** Semantic gameplay avatar identity for this exact player seat. */
  avatarIdentity?: PlayerAvatarIdentity | null;
  /** When set, the plate renders a fuchsia debug-highlight ring iff this
   *  player matches `useUiStore.debugHighlightedPlayerId`. Threaded through
   *  by both `PlayerHud` and `OpponentHud`; absence means the plate never
   *  participates in debug highlighting. */
  playerId?: PlayerId;
  density?: "default" | "compact";
}

const TONE_CLASSES: Record<HudTone, string> = {
  neutral: "border-[#9d9278]/30 text-stone-100",
  emerald: "border-emerald-300/36 text-emerald-50",
  rose: "border-rose-300/36 text-rose-50",
  cyan: "border-cyan-300/38 text-cyan-50",
  amber: "border-amber-300/36 text-amber-50",
};

const ACTIVE_TURN_CLASSES: Record<HudTone, string> = {
  neutral: "border-[#c9b98f]/48 ring-1 ring-[#d6c79f]/18",
  emerald: "border-emerald-300/52 ring-1 ring-emerald-300/28",
  rose: "border-rose-300/52 ring-1 ring-rose-300/28",
  cyan: "border-cyan-300/52 ring-1 ring-cyan-300/30",
  amber: "border-amber-300/52 ring-1 ring-amber-300/28",
};

export function HudPlate({
  label,
  hideLabel = false,
  tone = "neutral",
  onClick,
  children,
  trailing,
  cornerBadge,
  active = false,
  seatColor,
  underAttack = false,
  avatarIdentity,
  playerId,
  density = "default",
}: HudPlateProps) {
  const { t } = useTranslation("game");
  const avatar = usePlayerAvatarImage(avatarIdentity ?? null);
  const Component = onClick ? "button" : "div";
  const activeChrome = active ? ` ${ACTIVE_TURN_CLASSES[tone]}` : "";
  const isDebugHighlighted = useUiStore(
    (s) => playerId != null && s.debugHighlightedPlayerId === playerId,
  );
  const compact = density === "compact";
  const plateChrome = hideLabel
    ? compact
      ? "min-h-9 gap-1 px-1.5 py-0.5"
      : "min-h-11 gap-2 px-2 py-1 lg:gap-2.5 lg:px-3"
    : compact
      ? "min-h-11 gap-1 px-1.5 py-1"
      : "min-h-12 gap-2 px-2 py-1.5 lg:gap-2.5 lg:px-3 lg:py-2";
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
      aria-label={hideLabel && onClick ? label : undefined}
      data-hud-plate=""
      data-hud-plate-label-hidden={hideLabel ? "true" : undefined}
      data-hud-tone={tone}
      data-active-turn={active ? "true" : undefined}
      className={`tabletop-hud-plate group relative inline-flex max-w-full items-center border transition-[border-color,background-color,box-shadow] duration-150 ${plateChrome} ${TONE_CLASSES[tone]}${activeChrome} ${
        onClick ? "cursor-pointer hover:border-[#d4c69f]/48 hover:brightness-110" : ""
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
          className="pointer-events-none absolute inset-0 z-30 outline-2 outline-fuchsia-300"
        />
      )}
      <div
        aria-hidden
        className="pointer-events-none absolute inset-0"
        data-avatar-fallback={avatar.src ? undefined : "true"}
        data-hud-plate-art=""
        style={avatar.src || !seatColor ? undefined : { color: seatColor }}
      >
        {avatar.src ? (
          <img
            src={avatar.src}
            alt=""
            className="absolute inset-0 h-full w-full object-cover"
            onError={() => avatar.advanceFailedSource(avatar.src!)}
          />
        ) : (
          <svg
            viewBox="0 0 64 64"
            className="absolute inset-0 h-full w-full"
            fill="currentColor"
            data-hud-plate-profile-fallback=""
          >
            <circle cx="32" cy="23" r="12" />
            <path d="M10 59c1.4-14 9.3-22 22-22s20.6 8 22 22H10Z" />
          </svg>
        )}
      </div>
      {cornerBadge ? (
        <div
          className="absolute -top-0.5 left-1/2 z-40 -translate-x-1/2 -translate-y-1/2"
          data-hud-plate-corner=""
        >
          {cornerBadge}
        </div>
      ) : null}
      <div className="pointer-events-none absolute inset-[1px] border-t border-[#f5eac8]/10" />
      {avatar.src ? (
        <div className="contents" data-hud-plate-avatar="">
          <HudAvatar
            label={label}
            avatarUrl={avatar.src}
            seatColor={seatColor}
            compact={compact}
            onAvatarError={avatar.advanceFailedSource}
          />
        </div>
      ) : null}
      <div
        className={`relative flex min-w-0 flex-col items-center justify-center ${contentGap}`}
        data-hud-plate-content=""
      >
        {!hideLabel ? (
          <div
            className={`flex min-w-0 items-center justify-center ${contentGap}`}
            data-hud-plate-label=""
          >
            {!avatar.src && seatColor && (
              <span
                aria-hidden
                className={`${compact ? "h-2 w-2" : "h-2.5 w-2.5"} shrink-0 rounded-full ring-1 ring-black/30`}
                style={{ backgroundColor: seatColor }}
              />
            )}
            <span
              data-hud-plate-label-text=""
              className={labelClass}
              style={seatColor ? { color: seatColor } : { color: "rgba(255,255,255,0.68)" }}
            >
              {label}
            </span>
          </div>
        ) : null}
        <div
          className={`flex min-w-0 items-center justify-center ${childGap}`}
          data-hud-plate-primary=""
        >
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
  onAvatarError,
}: {
  label: string;
  avatarUrl: string;
  seatColor?: string;
  compact: boolean;
  onAvatarError(failedSrc: string): void;
}) {
  return (
    <AvatarHoverPreview
      avatarUrl={avatarUrl}
      label={label}
      seatColor={seatColor}
      title={label}
      className={`relative shrink-0 overflow-hidden rounded-full border border-white/15 bg-slate-950 shadow-[0_8px_18px_rgba(0,0,0,0.35)] ${compact ? "h-9 w-9" : "h-12 w-12 lg:h-14 lg:w-14"}`}
      style={seatColor ? {
        borderColor: `${seatColor}cc`,
        boxShadow: `0 0 0 1px ${seatColor}55, 0 10px 24px rgba(0,0,0,0.35)`,
      } : undefined}
      onAvatarError={onAvatarError}
    >
      <img
        src={avatarUrl}
        alt={label}
        className="h-full w-full object-cover"
        onError={() => onAvatarError(avatarUrl)}
      />
      <div className="absolute inset-0 bg-gradient-to-b from-white/12 via-transparent to-black/32" />
    </AvatarHoverPreview>
  );
}
