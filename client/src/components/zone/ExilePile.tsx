import { useGameStore } from "../../stores/gameStore.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { getPlayerZoneIds, getWaitingForObjectChoiceIds } from "../../viewmodel/gameStateView.ts";

interface ExilePileProps {
  playerId: number;
  onClick: () => void;
  size?: { width: string; height: string };
}

export function ExilePile({ playerId, onClick, size }: ExilePileProps) {
  const count = useGameStore((s) => getPlayerZoneIds(s.gameState, "exile", playerId).length);
  const canActForWaitingState = useCanActForWaitingState();
  const hasSelectableCards = useGameStore((s) => {
    if (!canActForWaitingState) return false;
    const objectChoiceIds = new Set(getWaitingForObjectChoiceIds(s.waitingFor));
    return getPlayerZoneIds(s.gameState, "exile", playerId).some((id) => objectChoiceIds.has(id));
  });

  if (count === 0) return null;

  const w = size?.width ?? "var(--card-w)";
  const h = size?.height ?? "var(--card-h)";

  return (
    <button
      onClick={onClick}
      className={`group relative cursor-pointer ${hasSelectableCards ? "ring-2 ring-amber-400/60 rounded-lg shadow-[0_0_12px_3px_rgba(201,176,55,0.8)]" : ""}`}
      title={`Exile (${count})`}
      style={{ width: w, height: h }}
    >
      <div className="relative h-full w-full overflow-hidden rounded-lg border border-indigo-500/40 shadow-md group-hover:border-indigo-400/60 transition-colors">
        <div
          className="absolute inset-0"
          style={{
            background: "radial-gradient(ellipse at center, rgba(99,102,241,0.25) 0%, rgba(15,10,30,0.95) 70%)",
          }}
        />
        <div
          className="absolute inset-0 animate-pulse"
          style={{
            background: "radial-gradient(circle at 50% 50%, rgba(139,92,246,0.3) 0%, transparent 50%)",
            animationDuration: "3s",
          }}
        />
        <div className="absolute inset-0 flex items-center justify-center">
          <span className="text-[10px] font-bold uppercase tracking-wider text-indigo-300/80">Exile</span>
        </div>
      </div>
      <div className="absolute -bottom-1 -right-1 z-10 flex h-5 w-5 items-center justify-center rounded-full bg-indigo-950 text-[9px] font-bold text-indigo-200 ring-1 ring-indigo-500/60">
        {count}
      </div>
    </button>
  );
}
