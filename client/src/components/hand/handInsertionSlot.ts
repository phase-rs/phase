export interface HandSlotRect {
  objectId: number;
  left: number;
  width: number;
  /** Viewport top of the card's rect — used to vertically place the drop arrow on the fan arc. */
  top?: number;
  /** Viewport height of the card's rect. */
  height?: number;
}

/**
 * Fraction of a card's width that the *visible* slot between the two flanking
 * cards should open to once they slide apart. The drop target reads as a real
 * gap you could drop a card into, not a hairline.
 */
export const VISIBLE_GAP_FRACTION = 2 / 3;

/**
 * Total displacement (px) that opens between the two cards flanking the drop
 * position. Each flank shifts by half this amount (rigid two-block model: the
 * whole left block shifts left by gapPx/2, the whole right block shifts right),
 * so the inter-card overlap is preserved and exactly one gap appears.
 *
 * Hand cards overlap at rest (negative margin), so the two-block model separates
 * the flanking pair by exactly `gapPx`, leaving a visible gap of
 * `gapPx - edgeOverlapPx`. To land that visible gap on `VISIBLE_GAP_FRACTION` of
 * the card width regardless of how tightly the hand is packed, the displacement
 * must also cover the resting overlap:
 *
 *   gapPx = VISIBLE_GAP_FRACTION * cardWidthPx + edgeOverlapPx
 *
 * `cardWidthPx` is the rendered (transform-free) card width and `edgeOverlapPx`
 * is the resting overlap between adjacent cards (the absolute negative margin),
 * both measured once at drag start.
 */
export function computeGapPx(cardWidthPx: number, edgeOverlapPx: number): number {
  return VISIBLE_GAP_FRACTION * cardWidthPx + edgeOverlapPx;
}

/**
 * Pure computation of the post-reorder hand order for a drag-and-drop within the
 * hand, or `null` when the reorder must be suppressed or is a no-op.
 *
 * The caller derives a `ReorderHand` order by mapping a *displayed* slot index
 * onto `hand` (the engine's `player.hand` order). That mapping is only 1:1 when
 * the displayed order equals `hand`, so the caller passes `suppressed = true`
 * whenever the display diverges from `hand`: a cast is in flight (the pending
 * card is filtered out of the DOM, leaving N-1 slots) or the hand is sorted /
 * filtered (the display permutes or hides entries). Dispatching a reorder in
 * those states would scramble the real hand — this returns `null` instead.
 * Returns `null` too for a no-op move (unknown slot, card not in `hand`, or the
 * card already at `targetSlot`). Generic over the id type so it is exercisable
 * with plain numbers in tests and `ObjectId`s in production.
 */
export function computeReorderedHand<Id>(
  hand: readonly Id[],
  objectId: Id,
  targetSlot: number | null,
  suppressed: boolean,
): Id[] | null {
  if (suppressed || targetSlot == null) return null;
  const order = hand.slice();
  const fromIdx = order.indexOf(objectId);
  if (fromIdx === -1 || fromIdx === targetSlot) return null;
  const [moved] = order.splice(fromIdx, 1);
  order.splice(targetSlot, 0, moved);
  return order;
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
 * Center POINT of the drop slot for a given insertion `slot`, as a viewport
 * (x, y). The caller anchors the arrow's tip here and tilts it about that tip,
 * so the arrow sits dead-center in the (tilted) gap corridor at every position.
 *
 * Works entirely in card-CENTER space, which is tilt-proof: the bounding box of
 * a rotated card is inflated and its left/right EDGES no longer match the visual
 * edges, but the box CENTER is still the card's true center. Using edges (the
 * old midpoint-of-facing-edges formula) drifts as the fan tilt grows toward the
 * ends; using centers does not.
 *
 * Operates in the drag-excluded ("remaining") space — the dragged card is
 * filtered out first.
 *
 * - Interior slot s: midpoint of the centers of the two flanking cards (their
 *   symmetric ±gapPx/2 slide keeps that midpoint fixed at the gap center).
 * - Edge slot (slot 0 / append): there is no second flank, so extrapolate half
 *   an inter-card step beyond the end card's center, continuing the fan's
 *   spacing and arc — the slot the dragged card will occupy.
 *
 * Returns null when no cards remain; the lone-card case returns that card's
 * center (no neighbor to define a direction).
 */
export function computeHandInsertionMarker(
  cards: HandSlotRect[],
  slot: number,
  draggingId: number,
): { x: number; y: number } | null {
  const remaining = cards.filter((card) => card.objectId !== draggingId);
  const n = remaining.length;
  if (n === 0) return null;
  const center = (c: HandSlotRect) => ({
    x: c.left + c.width / 2,
    y: (c.top ?? 0) + (c.height ?? 0) / 2,
  });
  if (n === 1) return center(remaining[0]);
  const clamped = Math.max(0, Math.min(slot, n));
  if (clamped === 0) {
    const c0 = center(remaining[0]);
    const c1 = center(remaining[1]);
    return { x: c0.x - (c1.x - c0.x) / 2, y: c0.y - (c1.y - c0.y) / 2 };
  }
  if (clamped >= n) {
    const cl = center(remaining[n - 1]);
    const cp = center(remaining[n - 2]);
    return { x: cl.x + (cl.x - cp.x) / 2, y: cl.y + (cl.y - cp.y) / 2 };
  }
  const a = center(remaining[clamped - 1]);
  const b = center(remaining[clamped]);
  return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
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
  gapPx: number,
): number {
  if (slot < 0 || draggingIndex < 0) return 0;
  if (index === draggingIndex) return 0;
  const remainingIndex = index < draggingIndex ? index : index - 1;
  return remainingIndex < slot ? -gapPx / 2 : gapPx / 2;
}

/**
 * The `handObjects`-space indices of the two cards flanking the gap at insertion
 * `slot` (drag-excluded space), or null on the side that has no card (slot 0 has
 * no left card; the append slot has no right card). Used to tilt the arrow to
 * the average of the flanking cards' fan rotations and to light the inner edge
 * of each flanking card.
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
