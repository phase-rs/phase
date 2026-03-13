const CARD_ASPECT_RATIO = 63 / 88; // ~0.716

interface CardSizeOptions {
  baseWidth?: number;
  minWidth?: number;
  gap?: number;
}

interface CardSize {
  width: number;
  height: number;
  gap: number;
}

export function getCardSize(
  containerWidth: number,
  cardCount: number,
  options: CardSizeOptions = {},
): CardSize {
  const { baseWidth = 100, minWidth = 50, gap = 8 } = options;

  if (cardCount === 0) {
    return { width: baseWidth, height: baseWidth / CARD_ASPECT_RATIO, gap };
  }

  const scaleFactor = Math.min(
    1,
    containerWidth / (cardCount * (baseWidth + gap)),
  );
  const width = Math.max(minWidth, Math.round(baseWidth * scaleFactor));
  const height = Math.round(width / CARD_ASPECT_RATIO);

  return { width, height, gap: Math.round(gap * scaleFactor) };
}

export function getStackCardSize(stackCount: number): CardSize {
  const baseWidth = 240;
  const minWidth = 140;

  if (stackCount <= 1) {
    return {
      width: baseWidth,
      height: Math.round(baseWidth / CARD_ASPECT_RATIO),
      gap: 10,
    };
  }

  // Shrink curve: exponential decay from 2+ items
  const shrinkFactor = Math.max(minWidth / baseWidth, 1.2 / Math.sqrt(stackCount));
  const width = Math.round(baseWidth * shrinkFactor);
  const height = Math.round(width / CARD_ASPECT_RATIO);

  return { width, height, gap: 6 };
}
