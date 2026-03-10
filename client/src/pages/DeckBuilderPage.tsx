import { useState } from "react";
import { useNavigate } from "react-router";

import { CardPreview } from "../components/card/CardPreview";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { DeckBuilder } from "../components/deck-builder/DeckBuilder";

export function DeckBuilderPage() {
  const navigate = useNavigate();
  const [hoveredCardName, setHoveredCardName] = useState<string | null>(null);

  return (
    <div className="h-screen bg-gray-950">
      <ScreenChrome onBack={() => navigate("/")} />
      <DeckBuilder onCardHover={setHoveredCardName} />
      <CardPreview cardName={hoveredCardName} />
    </div>
  );
}
