import { useState } from "react";

import { CardPreview } from "../components/card/CardPreview";
import { DeckBuilder } from "../components/deck-builder/DeckBuilder";

export function DeckBuilderPage() {
  const [hoveredCardName, setHoveredCardName] = useState<string | null>(null);

  return (
    <div className="h-screen bg-gray-950">
      <DeckBuilder onCardHover={setHoveredCardName} />
      <CardPreview cardName={hoveredCardName} />
    </div>
  );
}
