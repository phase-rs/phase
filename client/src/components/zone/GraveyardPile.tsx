import { useCardImage } from "../../hooks/useCardImage.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import type { TargetRef } from "../../adapter/types.ts";

const EMPTY: readonly number[] = [];

interface GraveyardPileProps {
  playerId: number;
  onClick: () => void;
}

function TopCard({ cardName }: { cardName: string }) {
  const { src } = useCardImage(cardName, { size: "normal" });

  if (!src) {
    return (
      <div className="h-full w-full rounded-lg bg-gray-700 border border-gray-600" />
    );
  }

  return (
    <img
      src={src}
      alt={cardName}
      className="h-full w-full rounded-lg object-cover"
      draggable={false}
    />
  );
}

export function GraveyardPile({ playerId, onClick }: GraveyardPileProps) {
  const graveyard = useGameStore(
    (s) => s.gameState?.players[playerId]?.graveyard ?? EMPTY,
  );
  const topCardName = useGameStore((s) => {
    const state = s.gameState;
    if (!state) return null;
    const gy = state.players[playerId]?.graveyard;
    if (!gy || gy.length === 0) return null;
    return state.objects[gy[gy.length - 1]]?.name ?? null;
  });

  // Check if any graveyard card is a valid target during targeting
  const currentPlayerId = usePlayerId();
  const hasTargetableCards = useGameStore((s) => {
    const wf = s.gameState?.waiting_for;
    if (!wf) return false;
    if (wf.type !== "TargetSelection" && wf.type !== "TriggerTargetSelection") return false;
    if (wf.data.player !== currentPlayerId) return false;
    const legalTargets: TargetRef[] = wf.data.selection.current_legal_targets;
    const gy = s.gameState?.players[playerId]?.graveyard ?? [];
    return gy.some((id) => legalTargets.some((t) => "Object" in t && t.Object === id));
  });

  const count = graveyard.length;
  if (count === 0) return null;

  const stackDepth = Math.min(count - 1, 3);

  return (
    <button
      onClick={onClick}
      className={`group relative cursor-pointer ${hasTargetableCards ? "ring-2 ring-amber-400/60 rounded-lg shadow-[0_0_12px_3px_rgba(201,176,55,0.8)]" : ""}`}
      title={`Graveyard (${count})`}
      style={{
        width: "var(--card-w)",
        height: "var(--card-h)",
      }}
    >
      {/* Shadow stack layers */}
      {Array.from({ length: stackDepth }).map((_, i) => (
        <div
          key={i}
          className="absolute rounded-lg border border-gray-600 bg-gray-800"
          style={{
            width: "var(--card-w)",
            height: "var(--card-h)",
            bottom: (i + 1) * 3,
            left: (i + 1) * -1,
          }}
        />
      ))}

      {/* Top card — full card image */}
      <div className="relative h-full w-full overflow-hidden rounded-lg border border-gray-500 shadow-md group-hover:border-gray-300 transition-colors">
        {topCardName && <TopCard cardName={topCardName} />}
        <div className="absolute inset-0 bg-black/20 group-hover:bg-black/0 transition-colors" />
      </div>

      {/* Count badge */}
      <div className="absolute -bottom-1 -right-1 z-10 flex h-5 w-5 items-center justify-center rounded-full bg-gray-900 text-[9px] font-bold text-gray-300 ring-1 ring-gray-600">
        {count}
      </div>
    </button>
  );
}
