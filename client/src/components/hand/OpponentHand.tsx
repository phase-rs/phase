import { useGameStore } from "../../stores/gameStore.ts";

export function OpponentHand() {
  const opponent = useGameStore((s) => s.gameState?.players[1]);

  if (!opponent) return null;

  const cardCount = opponent.hand.length;

  if (cardCount === 0) return null;

  return (
    <div className="flex items-center justify-center px-4 py-1">
      {Array.from({ length: cardCount }, (_, i) => (
        <div
          key={i}
          className="rounded-lg border border-gray-600 bg-gradient-to-br from-gray-800 via-gray-700 to-gray-800 shadow-md"
          style={{
            width: "calc(var(--card-w) * 0.6)",
            height: "calc(var(--card-h) * 0.6)",
            marginLeft: i > 0 ? "-14px" : undefined,
          }}
        >
          <div className="flex h-full items-center justify-center">
            <div className="h-[70%] w-[70%] rounded border border-gray-500 bg-gradient-to-br from-amber-900/40 via-amber-800/30 to-amber-900/40" />
          </div>
        </div>
      ))}
      {cardCount > 5 && (
        <span className="ml-2 rounded bg-gray-700 px-1.5 py-0.5 text-xs font-medium text-gray-300">
          {cardCount}
        </span>
      )}
    </div>
  );
}
