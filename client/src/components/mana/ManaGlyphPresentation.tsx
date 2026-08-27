import {
  useManaSymbolImage,
  type ManaSymbolShard,
} from "../../hooks/useFixedVisualImage.ts";

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
  if (isLoading) {
    return <span aria-label={notation} className={className} role="img" />;
  }
  return <span className={className}>{notation}</span>;
}
