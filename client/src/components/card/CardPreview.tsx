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
        right: 16,
        top: 16,
      };

  return (
    <div
      className="fixed z-[60] pointer-events-none"
      style={style}
    >
      {isLoading || !src ? (
        <div className="w-[472px] h-[659px] rounded-xl bg-gray-700 border border-gray-600 shadow-2xl animate-pulse" />
      ) : (
        <img
          src={src}
          alt={cardName}
          className="w-[472px] h-[659px] rounded-xl border border-gray-600 shadow-2xl object-cover"
          draggable={false}
        />
      )}
    </div>
  );
}
