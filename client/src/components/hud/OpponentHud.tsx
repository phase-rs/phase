import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { LifeTotal } from "../controls/LifeTotal.tsx";
import { ManaPoolSummary } from "./ManaPoolSummary.tsx";

interface OpponentHudProps {
  opponentName?: string | null;
}

export function OpponentHud({ opponentName }: OpponentHudProps) {
  const playerId = usePlayerId();
  const opponentId = playerId === 0 ? 1 : 0;
  const isOpponentTurn = useGameStore((s) => s.gameState?.active_player !== playerId);

  return (
    <div data-player-hud={opponentId} className="flex items-center justify-center py-1">
      <div className={`flex items-center gap-2 rounded-full px-3 py-1 transition-all duration-300 ${isOpponentTurn ? "bg-black/50 ring-[3px] ring-red-400 shadow-[0_0_20px_rgba(248,113,113,0.5),0_0_6px_rgba(248,113,113,0.4)]" : "bg-black/50"}`}>
        {opponentName && (
          <span className="text-xs font-medium text-gray-400">{opponentName}</span>
        )}
        <LifeTotal playerId={1} size="lg" />
        <ManaPoolSummary playerId={1} />
      </div>
    </div>
  );
}
