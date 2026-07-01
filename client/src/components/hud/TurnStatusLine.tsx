import { Trans, useTranslation } from "react-i18next";

import { useSeatColor } from "../../hooks/useSeatColor.ts";
import { useTurnStatus } from "../../hooks/useTurnStatus.ts";
import { getOpponentDisplayName } from "../../stores/multiplayerStore.ts";

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

  return (
    <div
      role="status"
      aria-live="polite"
      className={`pointer-events-none flex max-w-[min(22rem,calc(100vw-1.25rem))] items-center gap-1.5 rounded-full border px-3 py-1 text-[11px] font-medium tracking-wide shadow-[0_12px_32px_rgba(15,23,42,0.45)] backdrop-blur-xl ${tone} [@media(max-height:500px)]:px-2 [@media(max-height:500px)]:py-0.5 [@media(max-height:500px)]:text-[10px]`}
    >
      <span
        aria-hidden
        className={`h-2 w-2 shrink-0 rounded-full ${canIActNow ? "bg-emerald-300" : ""} ${waitingOnOpponent ? "animate-pulse" : ""}`}
        style={canIActNow ? undefined : { backgroundColor: waitingSeatColor }}
      />
      <span className="truncate">
        {canIActNow ? (
          reasonText
            ? t("status.yourPriorityReason", { reason: reasonText })
            : t("status.yourPriority")
        ) : (
          <Trans
            t={t}
            i18nKey={reasonText ? "status.waitingForReason" : "status.waitingFor"}
            values={{ player: getOpponentDisplayName(waitingSeatId), reason: reasonText }}
            components={{ player: <span style={{ color: waitingSeatColor }} /> }}
          />
        )}
      </span>
    </div>
  );
}
