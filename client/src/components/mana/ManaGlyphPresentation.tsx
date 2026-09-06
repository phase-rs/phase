import { useManaSymbolImage } from "../../hooks/useFixedVisualImage.ts";
import { manaSymbolSourceUrl, type ManaSymbolShard } from "../../services/scryfall.ts";

interface ManaGlyphPresentationProps {
  shard: ManaSymbolShard | null;
  notation: string;
  className: string;
}

export function ManaGlyphPresentation({
  shard,
  notation,
  className,
}: ManaGlyphPresentationProps) {
  const { src, isLoading, advanceFailedSource } = useManaSymbolImage(shard);

  if (src) {
    return (
      <img
        src={src}
        alt={notation}
        className={className}
        draggable={false}
        onError={() => advanceFailedSource?.(src)}
      />
    );
  }
  if (isLoading && shard) {
    return (
      <img
        src={manaSymbolSourceUrl(shard)}
        alt={notation}
        className={className}
        draggable={false}
      />
    );
  }
  return <span className={className}>{notation}</span>;
}
