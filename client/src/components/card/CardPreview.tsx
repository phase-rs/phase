import { useEffect, useState } from "react";

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
  const [pointerPosition, setPointerPosition] = useState<{ x: number; y: number } | null>(null);

  useEffect(() => {
    if (typeof window === "undefined") return undefined;

    function handlePointerMove(event: MouseEvent) {
      setPointerPosition({ x: event.clientX, y: event.clientY });
    }

    window.addEventListener("mousemove", handlePointerMove);
    return () => window.removeEventListener("mousemove", handlePointerMove);
  }, []);

  const previewWidth =
    typeof window === "undefined" ? 472 : Math.min(Math.max(window.innerWidth * 0.26, 220), 472);
  const previewHeight =
    typeof window === "undefined"
      ? 661
      : Math.min(window.innerHeight * 0.8, previewWidth * (7 / 5));
  const viewportWidth = typeof window === "undefined" ? 1440 : window.innerWidth;
  const viewportHeight = typeof window === "undefined" ? 900 : window.innerHeight;
  const gap = 20;

  const style: React.CSSProperties = position
    ? {
        left: Math.min(position.x + 16, window.innerWidth - 488),
        top: Math.min(position.y - 200, window.innerHeight - 736),
      }
    : pointerPosition
      ? {
          left:
            pointerPosition.x > viewportWidth / 2
              ? Math.max(16, pointerPosition.x - previewWidth - gap)
              : Math.min(pointerPosition.x + gap, viewportWidth - previewWidth - 16),
          top: Math.min(
            Math.max(16, pointerPosition.y - previewHeight / 2),
            viewportHeight - previewHeight - 16,
          ),
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
