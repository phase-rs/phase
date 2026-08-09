import { useEffect, useState } from "react";

import { useShiftHeld } from "../../hooks/useShiftHeld.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { CardPreview, type CardHoverInfo } from "./CardPreview.tsx";

interface HoverCardPreviewProps {
  card: CardHoverInfo | null;
  onDismiss?: () => void;
  mobileLayout?: "modal" | "compact";
}

/**
 * Applies the shared card-hover preferences to card-name preview surfaces such
 * as drafting and deck building. In-game object previews use GameCardPreview.
 */
export function HoverCardPreview({
  card,
  onDismiss,
  mobileLayout,
}: HoverCardPreviewProps) {
  const cardPreviewMode = usePreferencesStore((s) => s.cardPreviewMode);
  const cardPreviewHoverDelayMs = usePreferencesStore((s) => s.cardPreviewHoverDelayMs);
  const shiftHeld = useUiStore((s) => s.shiftHeld);
  const [visibleCard, setVisibleCard] = useState<CardHoverInfo | null>(null);

  useShiftHeld();

  useEffect(() => {
    if (card == null) {
      setVisibleCard(null);
      return undefined;
    }

    // Match uiStore.inspectObject: delay only the first desktop hover, so
    // scrubbing between cards stays responsive once a preview is open.
    if (
      cardPreviewMode === "shift"
      || cardPreviewHoverDelayMs === 0
      || visibleCard != null
    ) {
      setVisibleCard(card);
      return undefined;
    }

    const timerId = window.setTimeout(() => setVisibleCard(card), cardPreviewHoverDelayMs);
    return () => window.clearTimeout(timerId);
  }, [card, cardPreviewHoverDelayMs, cardPreviewMode, visibleCard]);

  const previewCard = cardPreviewMode === "shift" && !shiftHeld ? null : visibleCard;

  return (
    <CardPreview
      cardName={previewCard?.name ?? null}
      scryfallId={previewCard?.scryfallId}
      sourcePrinting={previewCard?.sourcePrinting}
      dockSide={cardPreviewMode === "side"}
      onDismiss={onDismiss}
      mobileLayout={mobileLayout}
    />
  );
}
