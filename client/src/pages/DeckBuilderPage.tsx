import { useState } from "react";

import { CardPreview } from "../components/card/CardPreview";
import { DeckBuilder } from "../components/deck-builder/DeckBuilder";
import type { DeckFormat } from "../components/deck-builder/FormatFilter";

export function DeckBuilderPage() {
  const [hoveredCardName, setHoveredCardName] = useState<string | null>(null);
  const [format, setFormat] = useState<DeckFormat>("standard");

  return (
    <div className="h-screen bg-gray-950">
      <DeckBuilder
        onCardHover={setHoveredCardName}
        format={format}
        onFormatChange={setFormat}
      />
      <CardPreview cardName={hoveredCardName} />
    </div>
  );
}
