import { useCardImage } from "../../hooks/useCardImage.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { CARD_BACK_URL } from "../../services/scryfall.ts";
import { useGameStore } from "../../stores/gameStore.ts";

interface LibraryPileProps {
  playerId: number;
  size?: { width: string; height: string };
}

function TopCard({ cardName }: { cardName: string }) {
  const { src } = useCardImage(cardName, { size: "normal" });

  if (!src) {
    return (
      <div
        className="h-full w-full rounded-lg bg-gray-700 border border-gray-600"
      />
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

export function LibraryPile({ playerId, size }: LibraryPileProps) {
  const myId = usePlayerId();
  const count = useGameStore(
    (s) => s.gameState?.players[playerId]?.library?.length ?? 0,
  );
  const canPeek = useGameStore(
    (s) =>
      playerId === myId &&
      (s.gameState?.players[playerId]?.can_look_at_top_of_library ?? false),
  );
  const isRevealed = useGameStore((s) => {
    const lib = s.gameState?.players[playerId]?.library;
    if (!lib || lib.length === 0) return false;
    return s.gameState?.revealed_cards?.includes(lib[0]) ?? false;
  });
  const topCardName = useGameStore((s) => {
    const lib = s.gameState?.players[playerId]?.library;
    if (!lib || lib.length === 0) return null;
    const topId = lib[0];
    // Show top card if player can peek (Future Sight) or if card is publicly revealed
    const peek =
      playerId === myId &&
      (s.gameState?.players[playerId]?.can_look_at_top_of_library ?? false);
    const revealed = s.gameState?.revealed_cards?.includes(topId) ?? false;
    if (!peek && !revealed) return null;
    // library[0] = top of library (engine convention from zones.rs)
    return s.gameState?.objects[topId]?.name ?? null;
  });

  if (count === 0) return null;

  const stackDepth = Math.min(count - 1, 4);
  const isPeeking = (canPeek || isRevealed) && topCardName;
  const w = size?.width ?? "var(--card-w)";
  const h = size?.height ?? "var(--card-h)";

  return (
    <div
      className="relative"
      title={`Library (${count})`}
      style={{ width: w, height: h }}
    >
      {/* Stack layers */}
      {Array.from({ length: stackDepth }).map((_, i) => (
        <div
          key={i}
          className="pointer-events-none absolute rounded-lg border border-gray-700 bg-gray-800"
          style={{
            width: w,
            height: h,
            bottom: (i + 1) * 3,
            left: (i + 1) * 1,
          }}
        />
      ))}

      {/* Top card */}
      <div
        className={`relative h-full w-full overflow-hidden rounded-lg border shadow-md ${
          isRevealed ? "border-amber-500" : isPeeking ? "border-cyan-600" : "border-gray-600"
        }`}
      >
        {isPeeking ? (
          <TopCard cardName={topCardName} />
        ) : (
          <img
            src={CARD_BACK_URL}
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
