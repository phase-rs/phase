import { useEffect, useState } from "react";

import { useShiftHeld } from "../../hooks/useShiftHeld.ts";
import { usePreferencesStore, type CardPreviewMode } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import {
  CardPreview,
  type CardHoverInfo,
  type CardPreviewDockPosition,
} from "./CardPreview.tsx";

interface HoverCardPreviewProps {
  card: CardHoverInfo | null;
  mode?: "none" | CardPreviewMode;
  hoverDelayMs?: number;
  onDismiss?: () => void;
  mobileLayout?: "modal" | "compact";
  /** Keep this surface's desktop preview at the side, independent of the
   * global game-board hover preference. */
  forceDockSide?: boolean;
  dockPosition?: CardPreviewDockPosition;
}

/**
 * Applies the shared card-hover preferences to card-name preview surfaces such
 * as drafting and deck building. In-game object previews use GameCardPreview.
 */
export function HoverCardPreview({
  card,
  mode,
  hoverDelayMs,
  onDismiss,
  mobileLayout,
  forceDockSide = false,
  dockPosition,
}: HoverCardPreviewProps) {
  const globalMode = usePreferencesStore((s) => s.cardPreviewMode);
  const globalHoverDelayMs = usePreferencesStore((s) => s.cardPreviewHoverDelayMs);
  const effectiveMode = mode ?? globalMode;
  const effectiveHoverDelayMs = hoverDelayMs ?? globalHoverDelayMs;
  const shiftHeld = useUiStore((s) => s.shiftHeld);
  const [visibleCard, setVisibleCard] = useState<CardHoverInfo | null>(null);

  useShiftHeld(effectiveMode === "shift");

  useEffect(() => {
    if (card == null || effectiveMode === "none") {
      setVisibleCard(null);
      return undefined;
    }

    // Match uiStore.inspectObject: delay only the first desktop hover, so
    // scrubbing between cards stays responsive once a preview is open.
    if (
      effectiveMode === "shift"
      || effectiveHoverDelayMs === 0
      || visibleCard != null
    ) {
      setVisibleCard(card);
      return undefined;
    }

    const timerId = window.setTimeout(() => setVisibleCard(card), effectiveHoverDelayMs);
    return () => window.clearTimeout(timerId);
  }, [card, effectiveHoverDelayMs, effectiveMode, visibleCard]);

  const previewCard = effectiveMode === "shift" && !shiftHeld ? null : visibleCard;

  useEffect(() => {
    if (visibleCard == null || onDismiss == null || typeof window === "undefined") {
      return undefined;
    }

    // Grid/list rows can be replaced while the pointer is over them, so React
    // never receives their pointerleave. Clear the deck-builder-owned state on
    // the next mouse move outside every registered hover source.
    const handlePointerMove = (event: PointerEvent) => {
      if (event.pointerType !== "mouse") return;
      if (
        event.target instanceof Element
        && event.target.closest("[data-card-preview]") != null
      ) return;
      if (document.querySelector("[data-deck-card-hover]:hover") == null) {
        onDismiss();
      }
    };
    window.addEventListener("pointermove", handlePointerMove);
    return () => window.removeEventListener("pointermove", handlePointerMove);
  }, [onDismiss, visibleCard]);

  return (
    <CardPreview
      cardName={previewCard?.name ?? null}
      scryfallId={previewCard?.scryfallId}
      sourcePrinting={previewCard?.sourcePrinting}
      dockSide={forceDockSide || effectiveMode === "side"}
      dockPosition={dockPosition}
      onDismiss={onDismiss}
      mobileLayout={mobileLayout}
    />
  );
}
