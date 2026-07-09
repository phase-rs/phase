import type { CSSProperties } from "react";
import { useTranslation } from "react-i18next";

import type { PlayerId } from "../../adapter/types.ts";
import { seatStatusKey } from "../../game/waitingForRegistry.ts";
import { useSeatColor } from "../../hooks/useSeatColor.ts";
import { useTurnStatus } from "../../hooks/useTurnStatus.ts";
import type { PlayerBattlefieldView } from "../../viewmodel/gameStateView.ts";
import { OpponentHand } from "../hand/OpponentHand.tsx";
import { ExilePile } from "../zone/ExilePile.tsx";
import { GraveyardPile } from "../zone/GraveyardPile.tsx";
import { LibraryPile } from "../zone/LibraryPile.tsx";
import { OpponentSeatHeader } from "./OpponentSeatHeader.tsx";
import { PlayerArea } from "./PlayerArea.tsx";

type ZoneName = "graveyard" | "exile" | "library";

interface OpponentSeatPaneProps {
  playerId: PlayerId;
  battlefieldView: PlayerBattlefieldView;
  showCards: boolean;
  onKickPlayer?: (playerId: PlayerId) => void;
  onViewZone: (zone: ZoneName, playerId: PlayerId) => void;
}

const zoneRailStyle = {
  "--card-w": "clamp(26px, 2.2vw, 38px)",
  "--card-h": "clamp(36px, 3.1vw, 53px)",
} as CSSProperties;

const pileSize = { width: "var(--card-w)", height: "var(--card-h)" };

export function OpponentSeatPane({
  playerId,
  battlefieldView,
  showCards,
  onKickPlayer,
  onViewZone,
}: OpponentSeatPaneProps) {
  const { t } = useTranslation("game");
  const seatColor = useSeatColor(playerId);
  const { activePlayerId, waitingSeatId, reason } = useTurnStatus();
  const isActiveTurn = activePlayerId === playerId;
  const isWaitingOnThisPlayer = waitingSeatId === playerId;
  const waitingReasonText = isWaitingOnThisPlayer
    ? t(seatStatusKey(reason), reason?.params)
    : null;
  const seatStyle = {
    "--card-size-scale": "0.62",
    "--card-w": "clamp(30px, 2.7vw, 58px)",
    "--card-h": "clamp(42px, 3.8vw, 81px)",
    "--game-seat-header-height": "3.5rem",
    "--game-seat-hand-peek": "calc(var(--card-h) * 0.56)",
    borderColor: isActiveTurn ? "rgba(253,164,175,0.7)" : `${seatColor}55`,
    boxShadow: isActiveTurn
      ? `inset 0 0 0 1px rgba(253,164,175,0.45), inset 0 -24px 34px rgba(127,29,29,0.34), 0 0 26px rgba(244,63,94,0.24), 0 0 18px ${seatColor}14`
      : `inset 0 0 0 1px ${seatColor}22, inset 0 -18px 28px rgba(0,0,0,0.35), 0 0 18px ${seatColor}12`,
  } as CSSProperties;
  const laneTintStyle = {
    background: `linear-gradient(180deg, ${seatColor}24 0%, ${seatColor}0f 38%, transparent 100%)`,
  } as CSSProperties;
  const laneAccentStyle = { backgroundColor: seatColor } as CSSProperties;

  return (
    <section
      className={`group/opponent-seat relative flex min-h-0 min-w-0 flex-1 flex-col overflow-visible border-x border-t transition-[background-color,border-color,box-shadow] duration-200 hover:bg-slate-900/58 focus-within:bg-slate-900/58 ${
        isActiveTurn ? "bg-rose-950/30" : "bg-slate-950/42"
      } ${isWaitingOnThisPlayer ? "ring-1 ring-amber-300/40" : ""}`}
      data-testid={`opponent-seat-pane-${playerId}`}
      style={seatStyle}
    >
      <div
        aria-hidden
        className="pointer-events-none absolute inset-0 opacity-65 transition-opacity duration-200 group-hover/opponent-seat:opacity-100 group-focus-within/opponent-seat:opacity-100"
        style={laneTintStyle}
      />
      <div
        aria-hidden
        className="pointer-events-none absolute inset-x-0 top-0 h-0.5 opacity-80 transition-opacity duration-200 group-hover/opponent-seat:opacity-100 group-focus-within/opponent-seat:opacity-100"
        style={laneAccentStyle}
      />
      <div className="absolute inset-x-1 top-1 z-40">
        <OpponentSeatHeader
          playerId={playerId}
          compact={false}
          onKickPlayer={onKickPlayer}
        />
      </div>
      {/* 1fr/auto/1fr grid keeps the hand fan dead-center regardless of the
          waiting chip: the chip anchors top-left in the left track (below the
          header, clear of the global menu cluster that overlays the first
          pane's header corner) and the track ends at the fan's left edge, so
          a long label truncates instead of colliding with the hand. */}
      {/* z-40 (matching the header's wrapper, but later in DOM order) lifts the
          peeking hand fan above the z-30 zone-pile rail and the header so its
          cards are the topmost hit-test target where they're visible — without
          it, `elementFromPoint` returns the header/piles and both the card
          `onMouseEnter` and usePreviewDismiss's poll see no `[data-card-hover]`,
          so the preview never opens. The container stays pointer-events-none and
          only the card <img>s re-enable pointer events, so gaps between cards
          still fall through to the header (click-to-target, life, kick). */}
      <div className="pointer-events-none absolute inset-x-0 top-[calc(0.25rem+var(--game-seat-header-height,2.25rem))] z-40 grid h-[var(--game-seat-hand-peek,2rem)] grid-cols-[1fr_auto_1fr] overflow-hidden">
        <div className="flex min-w-0 items-center justify-start pl-1.5">
          {waitingReasonText && (
            <span
              className="pointer-events-auto inline-flex min-w-0 items-center gap-1 rounded-full bg-amber-400/16 px-2 py-0.5 text-[10px] font-semibold text-amber-100 ring-1 ring-amber-300/30"
              data-testid={`seat-waiting-reason-${playerId}`}
              title={waitingReasonText}
            >
              <span aria-hidden className="h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-amber-300" />
              <span className="truncate">{waitingReasonText}</span>
            </span>
          )}
        </div>
        <div className="pointer-events-none -translate-y-[52%]">
          <OpponentHand playerId={playerId} showCards={showCards} layout="split" />
        </div>
      </div>
      <div className="relative z-30 flex min-h-0 min-w-0 flex-col px-1 pb-1 pt-[calc(var(--game-seat-header-height,2.25rem)+0.45rem)]">
        <div className="flex min-w-0 items-start justify-end gap-1">
          <div
            className="flex min-w-[calc(var(--card-w)*3+0.5rem)] shrink-0 items-start justify-end gap-1 overflow-visible"
            style={zoneRailStyle}
          >
            <ExilePile
              playerId={playerId}
              size={pileSize}
              onClick={() => onViewZone("exile", playerId)}
            />
            <LibraryPile
              playerId={playerId}
              size={pileSize}
              onView={() => onViewZone("library", playerId)}
            />
            <GraveyardPile
              playerId={playerId}
              size={pileSize}
              onClick={() => onViewZone("graveyard", playerId)}
            />
          </div>
        </div>
      </div>
      <div className="relative z-10 flex min-h-0 flex-1 flex-col overflow-visible pb-3 pt-0.5">
        <PlayerArea
          playerId={playerId}
          mode="focused"
          battlefieldView={battlefieldView}
          splitOverview
        />
      </div>
    </section>
  );
}
