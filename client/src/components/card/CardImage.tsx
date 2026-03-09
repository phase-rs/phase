import { useCardImage } from "../../hooks/useCardImage.ts";

interface CardImageProps {
  cardName: string;
  size?: "small" | "normal" | "large";
  faceIndex?: number;
  className?: string;
  tapped?: boolean;
  hasUnimplementedMechanics?: boolean;
}

export function CardImage({
  cardName,
  size = "normal",
  faceIndex,
  className = "",
  tapped = false,
  hasUnimplementedMechanics = false,
}: CardImageProps) {
  const { src, isLoading } = useCardImage(cardName, { size, faceIndex });

  const tappedStyle = tapped ? "rotate-[90deg] origin-center" : "";
  const baseClasses = `w-[var(--card-w)] h-[var(--card-h)] rounded-lg transition-transform duration-200 ${tappedStyle} ${className}`;

  if (isLoading || !src) {
    return (
      <div
        className={`${baseClasses} bg-gray-700 border border-gray-600 shadow-md animate-pulse`}
        aria-label={`Loading ${cardName}`}
      />
    );
  }

  return (
    <div className="relative inline-block">
      <img
        src={src}
        alt={cardName}
        className={`${baseClasses} border border-gray-600 shadow-md object-cover`}
        draggable={false}
      />
      {hasUnimplementedMechanics && (
        <span
          className="absolute top-0.5 left-0.5 bg-amber-500 text-black text-[8px] font-bold rounded-sm px-0.5 leading-tight"
          title="This card has mechanics not yet fully implemented"
        >
          !
        </span>
      )}
    </div>
  );
}
