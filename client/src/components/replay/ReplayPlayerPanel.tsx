import type { GameState, Player, PlayerId } from "../../adapter/types.ts";

interface ReplayPlayerPanelProps {
  player: Player;
  activePlayerId: PlayerId;
  state: GameState;
  side: "top" | "bottom";
}

export function ReplayPlayerPanel({
  player,
  activePlayerId,
  state,
  side,
}: ReplayPlayerPanelProps) {
  const isActive = player.id === state.active_player;
  const isViewer = player.id === activePlayerId;
  const label = isViewer ? "You" : `Player ${player.id + 1}`;

  const battlefieldCards = state.battlefield
    .map((id) => state.objects[id])
    .filter((obj) => obj && obj.controller === player.id);

  const creatureCount = battlefieldCards.filter((obj) =>
    obj.card_types?.core_types?.includes("Creature"),
  ).length;
  const nonCreatureCount = battlefieldCards.length - creatureCount;

  const lifeColor =
    player.life <= 5
      ? "text-red-400"
      : player.life <= 10
        ? "text-orange-400"
        : "text-green-400";

  return (
    <div
      className={`flex items-center gap-4 px-4 py-2 rounded-lg border ${
        isActive ? "border-[#8b5cf6] bg-[#1e1a2e]" : "border-[#333] bg-[#1a1a1a]"
      }`}
    >
      {/* Identity */}
      <div className="flex flex-col min-w-[80px]">
        <span className="text-xs text-[#888]">{side === "top" ? "↑" : "↓"} {label}</span>
        {isActive && (
          <span className="text-[10px] text-[#8b5cf6] font-medium">Active</span>
        )}
      </div>

      {/* Life */}
      <div className="flex flex-col items-center">
        <span className={`text-2xl font-bold ${lifeColor}`}>{player.life}</span>
        <span className="text-[10px] text-[#666]">Life</span>
      </div>

      {/* Zone counts */}
      <div className="flex gap-3 text-xs text-[#aaa]">
        <div className="flex flex-col items-center">
          <span className="text-white font-medium">{player.hand.length}</span>
          <span className="text-[#666]">Hand</span>
        </div>
        <div className="flex flex-col items-center">
          <span className="text-white font-medium">{player.library.length}</span>
          <span className="text-[#666]">Library</span>
        </div>
        <div className="flex flex-col items-center">
          <span className="text-white font-medium">{player.graveyard.length}</span>
          <span className="text-[#666]">GY</span>
        </div>
      </div>

      {/* Battlefield summary */}
      <div className="flex gap-2 text-xs text-[#aaa]">
        <div className="flex flex-col items-center">
          <span className="text-white font-medium">{creatureCount}</span>
          <span className="text-[#666]">Creatures</span>
        </div>
        <div className="flex flex-col items-center">
          <span className="text-white font-medium">{nonCreatureCount}</span>
          <span className="text-[#666]">Other</span>
        </div>
      </div>

      {/* Poison */}
      {player.poison_counters > 0 && (
        <div className="flex flex-col items-center text-xs">
          <span className="text-yellow-400 font-medium">{player.poison_counters}</span>
          <span className="text-[#666]">Poison</span>
        </div>
      )}

      {/* Energy */}
      {(player.energy ?? 0) > 0 && (
        <div className="flex flex-col items-center text-xs">
          <span className="text-cyan-400 font-medium">⚡{player.energy}</span>
          <span className="text-[#666]">Energy</span>
        </div>
      )}
    </div>
  );
}
