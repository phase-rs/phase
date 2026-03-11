import { useGameStore } from "../../stores/gameStore.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";

interface ZoneIndicatorProps {
  zone: "graveyard" | "exile";
  playerId: number;
  onClick: () => void;
}

const ZONE_LABELS: Record<string, string> = {
  graveyard: "GY",
  exile: "Exile",
};

export function ZoneIndicator({ zone, playerId, onClick }: ZoneIndicatorProps) {
  const myId = usePlayerId();
  const count = useGameStore((s) => {
    const state = s.gameState;
    if (!state) return 0;

    if (zone === "graveyard") {
      return state.players[playerId]?.graveyard.length ?? 0;
    }

    // Exile: filter by owner
    return state.exile.filter((id) => {
      const obj = state.objects[id];
      return obj?.owner === playerId;
    }).length;
  });

  return (
    <button
      onClick={onClick}
      className="cursor-pointer rounded bg-gray-800 px-2 py-0.5 text-xs text-gray-400 transition-colors hover:bg-gray-700 hover:text-gray-200"
    >
      {playerId !== myId ? "Opp " : ""}{ZONE_LABELS[zone]} ({count})
    </button>
  );
}
