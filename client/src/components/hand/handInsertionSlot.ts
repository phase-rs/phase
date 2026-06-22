export interface HandSlotRect {
  objectId: number;
  left: number;
  width: number;
  /** Viewport top of the card's rect — used to vertically place the drop caret on the fan arc. */
  top?: number;
  /** Viewport height of the card's rect. */
  height?: number;
}

export function computeHandInsertionSlot(
  cards: HandSlotRect[],
  clientX: number,
  draggingId: number,
): number | null {
  if (cards.length === 0) return null;

  const remaining = cards.filter((card) => card.objectId !== draggingId);
  for (let slot = 0; slot < remaining.length; slot++) {
    const card = remaining[slot];
    const center = card.left + card.width / 2;
    if (clientX < center) return slot;
  }

  return remaining.length;
}

/**
 * Screen position of the drop-position caret for a given insertion `slot`.
 *
 * Operates in the SAME drag-excluded ("remaining") space as
 * `computeHandInsertionSlot`: the dragged card is filtered out first, then the
 * caret sits at the leading (left) edge of the card now occupying `slot`, or
 * just after the last remaining card when `slot` is the append position.
 * Returns viewport coordinates; the caller converts to container-local space.
 * Returns null when no cards remain (the dragged card was the only one).
 */
export function computeHandInsertionMarker(
  cards: HandSlotRect[],
  slot: number,
  draggingId: number,
): { x: number; top: number; height: number } | null {
  const remaining = cards.filter((card) => card.objectId !== draggingId);
  if (remaining.length === 0) return null;
  const clamped = Math.max(0, Math.min(slot, remaining.length));
  const card = clamped < remaining.length ? remaining[clamped] : remaining[remaining.length - 1];
  const x = clamped < remaining.length ? card.left : card.left + card.width;
  return { x, top: card.top ?? 0, height: card.height ?? 0 };
}
