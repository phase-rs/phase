import { useMemo } from "react";

import type { PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { commandZoneLeaders } from "../../viewmodel/commanderColumn.ts";
import { CommanderCardZone } from "../zone/CommanderCardZone.tsx";

interface TabletopHandCommandZoneProps {
  playerId: PlayerId;
  seat: "player";
}

/**
 * Keeps public command-zone cards beside the hand without joining the hand fan.
 * CommanderCardZone remains the interaction authority for cast, commander tax,
 * commander ninjutsu, inspection, and mana-payment preview.
 */
export function TabletopHandCommandZone({
  playerId,
  seat,
}: TabletopHandCommandZoneProps) {
  const gameState = useGameStore((state) => state.gameState);
  const leaders = useMemo(
    () => (gameState ? commandZoneLeaders(gameState, playerId) : []),
    [gameState, playerId],
  );

  if (leaders.length === 0) return null;

  return (
    <div
      className="pointer-events-none relative ml-5 flex shrink-0 items-end overflow-visible"
      data-tabletop-hand-command-zone={seat}
      data-tabletop-hand-command-side="right"
    >
      <CommanderCardZone playerId={playerId} handPresentation />
    </div>
  );
}
