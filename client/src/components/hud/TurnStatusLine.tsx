import { useTranslation } from "react-i18next";

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

  // Nothing pending to narrate (between turns, mid-animation, game over).
  if (waitingSeatId == null) return null;

  const reasonText = reason ? t(reason.key, reason.params) : "";

  let text: string;
  if (canIActNow) {
    text = reasonText
      ? t("status.yourPriorityReason", { reason: reasonText })
      : t("status.yourPriority");
  } else {
    const name = getOpponentDisplayName(waitingSeatId);
    text = reasonText
      ? t("status.waitingForReason", { player: name, reason: reasonText })
      : t("status.waitingFor", { player: name });
  }

  // Your decision reads as a positive prompt; waiting on someone else reads as
  // a muted, patient state. The dot pulses only while we wait on another seat.
  const tone = canIActNow
    ? "border-emerald-400/40 bg-emerald-950/70 text-emerald-50"
    : "border-white/12 bg-slate-950/75 text-slate-200";
  const dotTone = canIActNow ? "bg-emerald-300" : "bg-amber-300";

  return (
    <div
      role="status"
      aria-live="polite"
      className={`pointer-events-none flex max-w-[min(22rem,calc(100vw-1.25rem))] items-center gap-1.5 rounded-full border px-3 py-1 text-[11px] font-medium tracking-wide shadow-[0_12px_32px_rgba(15,23,42,0.45)] backdrop-blur-xl ${tone} [@media(max-height:500px)]:px-2 [@media(max-height:500px)]:py-0.5 [@media(max-height:500px)]:text-[10px]`}
    >
      <span
        aria-hidden
        className={`h-2 w-2 shrink-0 rounded-full ${dotTone} ${waitingOnOpponent ? "animate-pulse" : ""}`}
      />
      <span className="truncate">{text}</span>
    </div>
  );
}
