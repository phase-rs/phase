import { useCardImage } from "../../hooks/useCardImage.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";

interface LibraryPileProps {
  playerId: number;
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

export function LibraryPile({ playerId }: LibraryPileProps) {
  const myId = usePlayerId();
  const count = useGameStore(
    (s) => s.gameState?.players[playerId]?.library?.length ?? 0,
  );
  const canPeek = useGameStore(
    (s) =>
      playerId === myId &&
      (s.gameState?.players[playerId]?.can_look_at_top_of_library ?? false),
  );
  const topCardName = useGameStore((s) => {
    if (!canPeek) return null;
    const lib = s.gameState?.players[playerId]?.library;
    if (!lib || lib.length === 0) return null;
    // library[0] = top of library (engine convention from zones.rs)
    return s.gameState?.objects[lib[0]]?.name ?? null;
  });

  if (count === 0) return null;

  const stackDepth = Math.min(count - 1, 4);
  const isPeeking = canPeek && topCardName;

  return (
    <div
      className="relative"
      title={`Library (${count})`}
      style={{
        width: "var(--card-w)",
        height: "var(--card-h)",
      }}
    >
      {/* Stack layers */}
      {Array.from({ length: stackDepth }).map((_, i) => (
        <div
          key={i}
          className="absolute rounded-lg border border-gray-700 bg-gray-800"
          style={{
            width: "var(--card-w)",
            height: "var(--card-h)",
            bottom: (i + 1) * 3,
            left: (i + 1) * 1,
          }}
        />
      ))}

      {/* Top card */}
      <div
        className={`relative h-full w-full overflow-hidden rounded-lg border shadow-md ${
          isPeeking ? "border-cyan-600" : "border-gray-600"
        }`}
      >
        {isPeeking ? (
          <TopCard cardName={topCardName} />
        ) : (
          <img
            src="/card-back.png"
            alt="Library"
            className="h-full w-full rounded-lg object-cover"
            draggable={false}
          />
        )}
      </div>

      {/* Count badge */}
      <div className="absolute -bottom-1 -right-1 z-10 flex h-5 w-5 items-center justify-center rounded-full bg-gray-900 text-[9px] font-bold text-gray-300 ring-1 ring-gray-600">
        {count}
      </div>
    </div>
  );
}
