import { useCardImage } from "../../hooks/useCardImage.ts";

interface CardImageProps {
  cardName: string;
  size?: "small" | "normal" | "large";
  faceIndex?: number;
  className?: string;
  tapped?: boolean;
}

export function CardImage({
  cardName,
  size = "normal",
  faceIndex,
  className = "",
  tapped = false,
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
    <img
      src={src}
      alt={cardName}
      className={`${baseClasses} border border-gray-600 shadow-md object-cover`}
      draggable={false}
    />
  );
}
