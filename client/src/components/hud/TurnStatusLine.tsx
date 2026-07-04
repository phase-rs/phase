import type { ReactNode } from "react";
import { Trans, useTranslation } from "react-i18next";

import { useSeatColor } from "../../hooks/useSeatColor.ts";
import { useTurnStatus } from "../../hooks/useTurnStatus.ts";
import { getOpponentDisplayName } from "../../stores/multiplayerStore.ts";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";

/**
 * Persistent one-line "who has priority / why" narration. Fills the gap where
 * the action rail goes quiet because the local player is waiting on someone
 * else — the engine knows exactly who and why; this just renders it.
 *
 * Reads the single `useTurnStatus()` authority. Framing is driven by
 * `canIActNow` (spectator- and turn-control-safe), never by a raw seat compare,
 * so spectators and Mindslaver-controlled turns get correct copy. Rendered as
 * an `aria-live` status region so the changing state is announced.
 */
export function TurnStatusLine() {
  const { t } = useTranslation("game");
  const { waitingSeatId, canIActNow, waitingOnOpponent, reason } = useTurnStatus();
  // Hook must run unconditionally (before the early return). Resolves NEUTRAL
  // for a null seat, which is never rendered thanks to the guard below.
  const waitingSeatColor = useSeatColor(waitingSeatId);

  // Nothing pending to narrate (between turns, mid-animation, game over).
  if (waitingSeatId == null) return null;

  const reasonText = reason ? t(reason.key, reason.params) : "";

  // Your decision reads as a positive prompt; waiting on someone else reads as
  // a muted, patient state. The pill's own border/background carries the state
  // axis; when waiting on another seat, the dot AND the player name carry that
  // seat's identity color (mirroring the HUD plate's dot+label convention) so
  // "waiting for X" is visually anchored to X's plate. `animate-pulse` keeps the
  // attention affordance regardless of hue. The name is colored inline via
  // <Trans> so the tint stays inside the localized sentence regardless of word
  // order — no sentence splitting, no new keys.
  const tone = canIActNow
    ? "border-emerald-400/40 bg-emerald-950/70 text-emerald-50"
    : "border-white/12 bg-slate-950/75 text-slate-200";

  // The pill carries WHO holds the pending decision; a small icon badge overlapping
  // its corner carries WHAT that decision is (attack / block / target / pay / …),
  // so the "who + what" reads at a glance without a long sentence. The badge
  // supersedes the old leading dot — it owns the seat-color tint + attention pulse
  // AND the reason glyph in one mark. The full sentence still lives in the
  // hover/focus tooltip (reusing the pre-existing verbose `*Reason` strings — no new
  // i18n keys), which only mounts when a reason exists. `group`/`relative` are
  // required for GameplayTooltip's CSS `group-hover` + the badge's absolute anchor,
  // so the pill can no longer be `pointer-events-none` — fine, it lives in the
  // interactive action rail, not over the board.
  const badgeTone = canIActNow
    ? "border-emerald-300/50 bg-emerald-400 text-emerald-950"
    : "border-white/20 bg-slate-900/95";

  return (
    <div
      role="status"
      aria-live="polite"
      className={`group relative flex max-w-[min(22rem,calc(100vw-1.25rem))] items-center rounded-full border px-2.5 py-1 text-[11px] font-medium tracking-wide shadow-[0_12px_32px_rgba(15,23,42,0.45)] backdrop-blur-xl ${tone} [@media(max-height:500px)]:px-2 [@media(max-height:500px)]:py-0.5 [@media(max-height:500px)]:text-[10px]`}
    >
      <span
        aria-hidden
        className={`pointer-events-none absolute -left-1.5 -top-1.5 z-10 flex h-4 w-4 items-center justify-center rounded-full border shadow-[0_2px_6px_rgba(0,0,0,0.45)] ${badgeTone} ${waitingOnOpponent ? "animate-pulse" : ""}`}
        style={canIActNow ? undefined : { color: waitingSeatColor, borderColor: `${waitingSeatColor}99` }}
      >
        <ReasonIcon reasonKey={reason?.key} />
      </span>
      <span className="truncate">
        {canIActNow ? (
          t("status.yourPriority")
        ) : (
          <Trans
            t={t}
            i18nKey="status.waitingFor"
            values={{ player: getOpponentDisplayName(waitingSeatId) }}
            components={{ player: <span style={{ color: waitingSeatColor }} /> }}
          />
        )}
      </span>
      {reasonText && (
        <GameplayTooltip className="w-56">
          {canIActNow ? (
            t("status.yourPriorityReason", { reason: reasonText })
          ) : (
            <Trans
              t={t}
              i18nKey="status.waitingForReason"
              values={{ player: getOpponentDisplayName(waitingSeatId), reason: reasonText }}
              components={{ player: <span style={{ color: waitingSeatColor }} /> }}
            />
          )}
        </GameplayTooltip>
      )}
    </div>
  );
}

/**
 * Glyphs for the overlap badge, keyed by the `status.reason.*` suffix. Each is a
 * stroke-based 16-viewBox fragment (matching the app's inline-SVG convention) so
 * it inherits the badge's `currentColor` tint. The map groups reasons that share
 * a visual (all priority windows → hourglass; both trigger/stack decisions →
 * layers); anything unmapped falls back to the hourglass, mirroring
 * `waitingForReason`'s own graceful default so a new engine variant degrades to a
 * generic "waiting" mark rather than a blank badge.
 */
const REASON_ICON_PATHS: Record<string, ReactNode> = {
  // Sword — declaring attackers / combat priority.
  declareAttackers: (
    <>
      <path d="M13.5 2.5 L6.5 9.5" />
      <path d="M4 12 L7 9" />
      <path d="M2.5 13.5 L4.2 11.8" />
    </>
  ),
  // Shield — declaring blockers.
  declareBlockers: <path d="M8 2 L13 4 V8 C13 11 8 14 8 14 C8 14 3 11 3 8 V4 Z" />,
  // Flame — assigning combat damage.
  assigningDamage: (
    <path d="M8 2 C10 5 11 6.2 11 9 A3 3 0 0 1 5 9 C5 7.6 5.6 6.8 6.4 6 C6.6 7.4 7.4 7.6 8 6.8 C8.4 5.4 8 3.4 8 2 Z" />
  ),
  // Crosshair — choosing targets.
  choosingTargets: (
    <>
      <circle cx="8" cy="8" r="4.5" />
      <path d="M8 1 V3.5 M8 12.5 V15 M1 8 H3.5 M12.5 8 H15" />
      <circle cx="8" cy="8" r="0.9" fill="currentColor" stroke="none" />
    </>
  ),
  // Droplet — paying a cost.
  payingCost: <path d="M8 2 C8 2 4 7 4 10 A4 4 0 0 0 12 10 C12 7 8 2 8 2 Z" />,
  // Circular arrow — mulligan.
  mulligan: (
    <>
      <path d="M12.5 6 A5 5 0 1 0 13 9.5" />
      <path d="M12.5 2.5 V6 H9" />
    </>
  ),
  // Card with a down arrow — discarding.
  discarding: (
    <>
      <rect x="4.5" y="2.5" width="7" height="7.5" rx="1" />
      <path d="M8 11.5 V14.5 M6.5 13 L8 14.5 L9.5 13" />
    </>
  ),
  // Stacked layers — ordering triggers / responding to the stack.
  orderingTriggers: (
    <>
      <path d="M8 2 L14 5 L8 8 L2 5 Z" />
      <path d="M2 8 L8 11 L14 8" />
      <path d="M2 11 L8 14 L14 11" />
    </>
  ),
  // Hourglass — generic priority windows / thinking.
  priority: (
    <path d="M4 2 H12 M4 14 H12 M4.5 2 C4.5 5 8 7 8 8 C8 9 4.5 11 4.5 14 M11.5 2 C11.5 5 8 7 8 8 C8 9 11.5 11 11.5 14" />
  ),
};

// Reasons that share a glyph with a canonical key above.
REASON_ICON_PATHS.priorityCombat = REASON_ICON_PATHS.declareAttackers;
REASON_ICON_PATHS.respondingToStack = REASON_ICON_PATHS.orderingTriggers;
REASON_ICON_PATHS.priorityMain = REASON_ICON_PATHS.priority;
REASON_ICON_PATHS.thinking = REASON_ICON_PATHS.priority;

/** Reason glyph for the overlap badge. Strips the `status.reason.` namespace and
 *  looks up the shared path set; unmapped or absent reasons fall back to the
 *  generic hourglass. Purely decorative — the words live in the tooltip. */
function ReasonIcon({ reasonKey }: { reasonKey?: string }) {
  const name = reasonKey?.replace("status.reason.", "") ?? "";
  const glyph = REASON_ICON_PATHS[name] ?? REASON_ICON_PATHS.priority;
  return (
    <svg
      aria-hidden
      viewBox="0 0 16 16"
      className="h-2.5 w-2.5"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {glyph}
    </svg>
  );
}
