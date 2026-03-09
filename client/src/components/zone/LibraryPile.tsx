import { useGameStore } from "../../stores/gameStore.ts";

interface LibraryPileProps {
  playerId: number;
}

export function LibraryPile({ playerId }: LibraryPileProps) {
  const count = useGameStore((s) => s.gameState?.players[playerId]?.library_size ?? 0);

  if (count === 0) return null;

  const stackDepth = Math.min(count - 1, 4);

  return (
    <div
      className="relative"
      title={`Library (${count})`}
      style={{ width: 48, height: 36 }}
    >
      {/* Stack layers */}
      {Array.from({ length: stackDepth }).map((_, i) => (
        <div
          key={i}
          className="absolute rounded border border-gray-700 bg-gray-800"
          style={{
            width: 48,
            height: 36,
            bottom: (i + 1) * 2,
            left: (i + 1) * 1,
          }}
        />
      ))}

      {/* Top facedown card */}
      <div className="relative h-full w-full overflow-hidden rounded border-2 border-gray-600">
        {/* Card back pattern */}
        <div className="flex h-full w-full items-center justify-center bg-gradient-to-br from-indigo-950 to-gray-900">
          <div className="h-[70%] w-[70%] rounded-sm border border-amber-700/40 bg-gradient-to-br from-amber-900/30 to-indigo-900/30" />
        </div>
      </div>

      {/* Count badge */}
      <div className="absolute -bottom-1 -right-1 z-10 flex h-4 w-4 items-center justify-center rounded-full bg-gray-900 text-[8px] font-bold text-gray-300 ring-1 ring-gray-600">
        {count}
      </div>
    </div>
  );
}
