import { useMemo, useState } from "react";
import { useSearchParams } from "react-router";

import { CardPreview } from "../components/card/CardPreview";
import { DeckBuilder } from "../components/deck-builder/DeckBuilder";
import type { DeckFormat } from "../components/deck-builder/FormatFilter";

export function DeckBuilderPage() {
  const [searchParams] = useSearchParams();
  const [hoveredCardName, setHoveredCardName] = useState<string | null>(null);
  const [format, setFormat] = useState<DeckFormat>("standard");
  const initialDeckName = searchParams.get("create") === "1"
    ? null
    : searchParams.get("deck");

  const backPath = useMemo(() => {
    const returnTo = searchParams.get("returnTo");
    if (!returnTo) return "/";
    if (!returnTo.startsWith("/") || returnTo.startsWith("//")) return "/";
    return returnTo;
  }, [searchParams]);

  return (
    <div className="menu-scene h-screen overflow-hidden">
      <DeckBuilder
        onCardHover={setHoveredCardName}
        format={format}
        onFormatChange={setFormat}
        initialDeckName={initialDeckName}
        backPath={backPath}
      />
      <CardPreview cardName={hoveredCardName} />
    </div>
  );
}
