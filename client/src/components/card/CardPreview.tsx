import { useCardImage } from "../../hooks/useCardImage.ts";

interface CardPreviewProps {
  cardName: string | null;
  faceIndex?: number;
  position?: { x: number; y: number };
}

export function CardPreview({
  cardName,
  faceIndex,
  position,
}: CardPreviewProps) {
  if (!cardName) return null;

  return (
    <CardPreviewInner
      cardName={cardName}
      faceIndex={faceIndex}
      position={position}
    />
  );
}

function CardPreviewInner({
  cardName,
  faceIndex,
  position,
}: {
  cardName: string;
  faceIndex?: number;
  position?: { x: number; y: number };
}) {
  const { src, isLoading } = useCardImage(cardName, {
    size: "normal",
    faceIndex,
  });

  const style: React.CSSProperties = position
    ? {
        left: Math.min(position.x + 16, window.innerWidth - 488),
        top: Math.min(position.y - 200, window.innerHeight - 736),
      }
    : {
        right: "calc(env(safe-area-inset-right) + 1rem + var(--game-right-rail-offset, 0px))",
        top: "calc(env(safe-area-inset-top) + var(--game-top-overlay-offset, 0px) + 1rem)",
      };

  return (
    <div
      className="fixed z-[60] pointer-events-none"
      style={style}
    >
      {isLoading || !src ? (
        <div className="max-h-[80vh] max-w-[42vw] w-[clamp(220px,26vw,472px)] aspect-[5/7] rounded-xl border border-gray-600 bg-gray-700 shadow-2xl animate-pulse md:max-w-[45vw]" />
      ) : (
        <img
          src={src}
          alt={cardName}
          className="max-h-[80vh] max-w-[42vw] w-[clamp(220px,26vw,472px)] rounded-xl border border-gray-600 object-cover shadow-2xl md:max-w-[45vw]"
          draggable={false}
        />
      )}
    </div>
  );
}
