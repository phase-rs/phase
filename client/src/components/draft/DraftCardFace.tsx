import { useEffect, useState } from "react";

import { useCardImage, type SourcePrinting } from "../../hooks/useCardImage";
import { resolveAlternateCardFaceSync } from "../../services/scryfall";

export function useDraftCardFace(
  cardName: string,
  sourcePrinting: SourcePrinting | undefined,
) {
  const alternateFace = resolveAlternateCardFaceSync(cardName);
  const [showAlternate, setShowAlternate] = useState(false);
  useEffect(() => setShowAlternate(false), [cardName]);
  const activeAlternate = showAlternate && alternateFace !== null && alternateFace !== undefined;
  const image = useCardImage(cardName, {
    size: "normal",
    faceIndex: activeAlternate ? alternateFace.faceIndex : 0,
    sourcePrinting,
  });

  return {
    ...image,
    displayName: activeAlternate ? alternateFace.name : cardName,
    hasAlternateFace: alternateFace !== null && alternateFace !== undefined,
    toggleFace: () => setShowAlternate((current) => !current),
  };
}
