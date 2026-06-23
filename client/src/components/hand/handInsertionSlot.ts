export interface HandSlotRect {
  objectId: number;
  left: number;
  width: number;
  /** Viewport top of the card's rect — used to vertically place the drop caret on the fan arc. */
  top?: number;
  /** Viewport height of the card's rect. */
  height?: number;
}

/**
 * Total width of the gap that opens between the two cards flanking the drop
 * position. Each flank shifts by half this amount (rigid two-block model: the
 * whole left block shifts left by gapPx/2, the whole right block shifts right),
 * so the inter-card overlap is preserved and exactly one gap appears.
 */
export const INSERTION_GAP_PX = 32;

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
 * `computeHandInsertionSlot`: the dragged card is filtered out first. For an
 * interior slot the caret sits at the MIDPOINT between the trailing edge of the
 * card now before `slot` and the leading edge of the card now at `slot` — i.e.
 * the center of the gap the flanking cards open (symmetric displacement keeps
 * that center fixed at the resting midpoint). For slot 0 it sits at the leading
 * edge of the first remaining card; for the append slot, just past the last.
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
  if (clamped === 0) {
    const first = remaining[0];
    return { x: first.left, top: first.top ?? 0, height: first.height ?? 0 };
  }
  if (clamped >= remaining.length) {
    const last = remaining[remaining.length - 1];
    return { x: last.left + last.width, top: last.top ?? 0, height: last.height ?? 0 };
  }
  const leftCard = remaining[clamped - 1];
  const rightCard = remaining[clamped];
  const x = (leftCard.left + leftCard.width + rightCard.left) / 2;
  return { x, top: rightCard.top ?? 0, height: rightCard.height ?? 0 };
}

/**
 * Signed horizontal offset (px) to displace the hand card at `handObjects`
 * index `index` so a gap opens at insertion `slot`. Rigid two-block model:
 * every card whose drag-excluded ("remaining") index is left of `slot` shifts
 * by -gapPx/2; every card at or right of `slot` shifts by +gapPx/2. Returns 0
 * when no slot is active (`slot < 0` or `draggingIndex < 0`) and for the dragged
 * card itself (it follows the pointer, so it must not be displaced).
 */
export function computeFlankDisplacement(
  index: number,
  slot: number,
  draggingIndex: number,
  gapPx: number = INSERTION_GAP_PX,
): number {
  if (slot < 0 || draggingIndex < 0) return 0;
  if (index === draggingIndex) return 0;
  const remainingIndex = index < draggingIndex ? index : index - 1;
  return remainingIndex < slot ? -gapPx / 2 : gapPx / 2;
}

/**
 * The `handObjects`-space indices of the two cards flanking the gap at insertion
 * `slot` (drag-excluded space), or null on the side that has no card (slot 0 has
 * no left card; the append slot has no right card). Used to tilt the caret to
 * the average of the flanking cards' fan rotations.
 */
export function flankingHandIndices(
  slot: number,
  draggingIndex: number,
  handSize: number,
): { left: number | null; right: number | null } {
  const remainingLen = handSize - 1;
  const toHandIndex = (remainingIndex: number) =>
    remainingIndex < draggingIndex ? remainingIndex : remainingIndex + 1;
  const left = slot - 1 >= 0 ? toHandIndex(slot - 1) : null;
  const right = slot < remainingLen ? toHandIndex(slot) : null;
  return { left, right };
}
