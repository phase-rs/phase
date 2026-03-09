import { useCardImage } from "../../hooks/useCardImage.ts";
import { useGameStore } from "../../stores/gameStore.ts";

interface GraveyardPileProps {
  playerId: number;
  onClick: () => void;
}

function TopCardArt({ cardName }: { cardName: string }) {
  const { src } = useCardImage(cardName, { size: "art_crop" });

  if (!src) {
    return <div className="h-full w-full rounded bg-gray-700" />;
  }

  return (
    <img
      src={src}
      alt={cardName}
      className="h-full w-full rounded object-cover"
      draggable={false}
    />
  );
}

export function GraveyardPile({ playerId, onClick }: GraveyardPileProps) {
  const graveyard = useGameStore((s) => s.gameState?.players[playerId]?.graveyard ?? []);
  const topCardName = useGameStore((s) => {
    const state = s.gameState;
    if (!state) return null;
    const gy = state.players[playerId]?.graveyard;
    if (!gy || gy.length === 0) return null;
    return state.objects[gy[gy.length - 1]]?.name ?? null;
  });

  const count = graveyard.length;
  if (count === 0) return null;

  // Stacked card effect — offset layers beneath top card
  const stackDepth = Math.min(count - 1, 3);

  return (
    <button
      onClick={onClick}
      className="group relative cursor-pointer"
      title={`Graveyard (${count})`}
      style={{ width: 48, height: 36 }}
    >
      {/* Shadow stack layers */}
      {Array.from({ length: stackDepth }).map((_, i) => (
        <div
          key={i}
          className="absolute rounded border border-gray-600 bg-gray-800"
          style={{
            width: 48,
            height: 36,
            bottom: (i + 1) * 2,
            left: (i + 1) * -1,
          }}
        />
      ))}

      {/* Top card */}
      <div className="relative h-full w-full overflow-hidden rounded border-2 border-gray-500 group-hover:border-gray-300 transition-colors">
        {topCardName && <TopCardArt cardName={topCardName} />}
        <div className="absolute inset-0 bg-black/30 group-hover:bg-black/10 transition-colors" />
      </div>

      {/* Count badge */}
      <div className="absolute -bottom-1 -right-1 z-10 flex h-4 w-4 items-center justify-center rounded-full bg-gray-900 text-[8px] font-bold text-gray-300 ring-1 ring-gray-600">
        {count}
      </div>
    </button>
  );
}
