import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { LifeTotal } from "../controls/LifeTotal.tsx";
import { ManaPoolSummary } from "./ManaPoolSummary.tsx";

interface OpponentHudProps {
  opponentName?: string | null;
}

export function OpponentHud({ opponentName }: OpponentHudProps) {
  const playerId = usePlayerId();
  const focusedOpponent = useUiStore((s) => s.focusedOpponent);
  const gameState = useGameStore((s) => s.gameState);

  // In multiplayer, show the focused opponent; default to first non-self player
  const opponents = gameState
    ? (gameState.seat_order ?? gameState.players.map((p) => p.id)).filter(
        (id) => id !== playerId && !(gameState.eliminated_players ?? []).includes(id),
      )
    : [];
  const opponentId = focusedOpponent ?? opponents[0] ?? (playerId === 0 ? 1 : 0);

  const isOpponentTurn = useGameStore((s) => s.gameState?.active_player === opponentId);

  const label = opponentName ?? `Opp ${opponentId + 1}`;

  return (
    <div data-player-hud={String(opponentId)} className="flex items-center justify-center py-1">
      <div className={`flex items-center gap-2 rounded-full px-3 py-1 transition-all duration-300 ${isOpponentTurn ? "bg-black/50 ring-[3px] ring-red-400 shadow-[0_0_20px_rgba(248,113,113,0.5),0_0_6px_rgba(248,113,113,0.4)]" : "bg-black/50"}`}>
        <span className="text-xs font-medium text-gray-400">{label}</span>
        <LifeTotal playerId={opponentId} size="lg" hideLabel />
        <ManaPoolSummary playerId={opponentId} />
      </div>
    </div>
  );
}
