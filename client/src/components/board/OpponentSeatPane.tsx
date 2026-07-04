import type { CSSProperties } from "react";

import type { PlayerId } from "../../adapter/types.ts";
import { useSeatColor } from "../../hooks/useSeatColor.ts";
import type { PlayerBattlefieldView } from "../../viewmodel/gameStateView.ts";
import { OpponentHand } from "../hand/OpponentHand.tsx";
import { ExilePile } from "../zone/ExilePile.tsx";
import { GraveyardPile } from "../zone/GraveyardPile.tsx";
import { LibraryPile } from "../zone/LibraryPile.tsx";
import { PlayerArea } from "./PlayerArea.tsx";

type ZoneName = "graveyard" | "exile" | "library";

interface OpponentSeatPaneProps {
  playerId: PlayerId;
  battlefieldView: PlayerBattlefieldView;
  showCards: boolean;
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
  onViewZone,
}: OpponentSeatPaneProps) {
  const seatColor = useSeatColor(playerId);
  const seatStyle = {
    "--card-size-scale": "0.52",
    "--card-w": "clamp(28px, 2.35vw, 40px)",
    "--card-h": "clamp(39px, 3.3vw, 56px)",
    borderColor: `${seatColor}55`,
    boxShadow: `inset 0 0 0 1px ${seatColor}22, inset 0 -18px 28px rgba(0,0,0,0.35), 0 0 18px ${seatColor}12`,
  } as CSSProperties;
  const laneTintStyle = {
    background: `linear-gradient(180deg, ${seatColor}24 0%, ${seatColor}0f 38%, transparent 100%)`,
  } as CSSProperties;
  const laneAccentStyle = { backgroundColor: seatColor } as CSSProperties;

  return (
    <section
      className="group/opponent-seat relative flex min-h-0 min-w-0 flex-1 flex-col overflow-visible border-x border-y bg-slate-950/42 transition-[background-color,border-color,box-shadow] duration-200 hover:bg-slate-900/58 focus-within:bg-slate-900/58"
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
      <div
        aria-hidden
        className="pointer-events-none absolute inset-x-0 bottom-0 h-px opacity-55 transition-opacity duration-200 group-hover/opponent-seat:opacity-90 group-focus-within/opponent-seat:opacity-90"
        style={laneAccentStyle}
      />
      <div className="pointer-events-none absolute inset-x-0 top-0 z-20 h-[calc(var(--card-h)*0.58)] overflow-hidden">
        <div className="pointer-events-auto -translate-y-[34%]">
          <OpponentHand playerId={playerId} showCards={showCards} layout="split" />
        </div>
      </div>
      <div className="relative z-30 flex min-h-0 min-w-0 flex-col px-1 py-1">
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
      <div className="relative z-10 min-h-0 flex-1 overflow-visible pb-3">
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
