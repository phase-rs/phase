// Shared card-fan geometry — the overlap / tilt / arc math that lays a row of
// cards out as a held "hand". Extracted from PlayerHand so any centered spread
// of cards can share the same geometry building block while choosing the
// density appropriate to its surface: compact for constrained overlays/mobile,
// wide for the desktop player hand where adjacent hover targets matter.

export type FanGeometryProfile = "compact" | "wide";

// Signed overlap FRACTION of one card width by which each card slides over the
// previous one (negative == leftward). Tightens continuously as the row grows so
// a Commander-sized hand (up to ~20 cards) still fits on screen. Single source of
// truth for both the CSS margin (`getHandOverlap`) and the fan's total-width
// budget (`spreadFactor`), so a caller sizing cards to fit a viewport can never
// drift out of sync with the margin the cards actually render with.
function overlapFraction(rowSize: number, profile: FanGeometryProfile): number {
  if (profile === "wide") {
    if (rowSize <= 3) return -0.1;
    if (rowSize <= 5) return -0.15;
    if (rowSize <= 7) return -0.25;
    // For 8+ cards, target total width ≈ 5.5× card width. This exposes
    // substantially more of each adjacent card than the compact 4× profile,
    // while the lower clamp still reins in unusually large Commander hands.
    return Math.max(-0.86, Math.min(-0.35, 4.5 / (rowSize - 1) - 1));
  }

  if (rowSize <= 5) return -0.25;
  if (rowSize <= 7) return -0.45;
  // For 8+ cards: target total width ≈ 4× card width.
  // First card occupies 1w; remaining (n-1) each contribute (1 + overlap)w.
  // (n-1)(1 + overlap) = 3  =>  overlap = 3/(n-1) - 1, clamped to [-0.85, -0.6].
  return Math.max(-0.85, Math.min(-0.6, 3 / (rowSize - 1) - 1));
}

// Total width of the whole fan, in units of ONE card width: the first card
// occupies 1w and each of the remaining (n-1) cards adds its visible fraction
// `(1 + overlap)`. A caller sizing cards to fit a viewport divides its width
// budget by this factor. Never below 1 (a single card is 1w).
export function spreadFactor(
  rowSize: number,
  profile: FanGeometryProfile = "compact",
): number {
  if (rowSize <= 1) return 1;
  return 1 + (rowSize - 1) * (1 + overlapFraction(rowSize, profile));
}

// Horizontal overlap between adjacent fanned cards, as a CSS margin-left. The
// margin is a fraction of the card's OWN rendered width var (`cardWidthVar` —
// `--hand-card-w` for the hand, `--fan-card-w` for the attachment fan). This
// MUST match the width the cards actually render at: using a different basis
// (e.g. base `--card-w` while cards render 1.14–1.4× larger) leaves the real
// overlap off by the scale factor, spreading the fan ~40% too wide with the
// error compounding as the row grows.
export function getHandOverlap(
  rowSize: number,
  cardWidthVar = "--hand-card-w",
  profile: FanGeometryProfile = "compact",
): string {
  return `calc(var(${cardWidthVar}) * ${overlapFraction(rowSize, profile)})`;
}

// Quadratic arc lift coefficient. Scales down as the row grows so the parabola
// stays inside the band instead of pushing edge cards off-screen.
export function getArcCoefficient(
  rowSize: number,
  profile: FanGeometryProfile = "compact",
): number {
  if (profile === "wide") {
    if (rowSize <= 7) return 3.5;
    // Flatter desktop arc: keep the outermost drop around 32px.
    const maxDist = (rowSize - 1) / 2;
    return 32 / (maxDist * maxDist);
  }

  if (rowSize <= 7) return 6;
  // Keep max arc lift (at the edges) roughly constant at ~54px.
  const maxDist = (rowSize - 1) / 2;
  return 54 / (maxDist * maxDist);
}

/** Per-card fan placement functions, all sized by the total card count so a
 *  small row and a large row tuck into the same angle-clamped arc. `k` is a
 *  card's 0-based position across the row. */
export interface FanGeometry {
  /** Negative CSS margin-left overlapping each card over the previous one. */
  overlap: string;
  /** Signed tilt (deg) for the card at position `k`. */
  rotation: (k: number) => number;
  /** Downward-parabola vertical offset (px) for the card at position `k`. */
  arc: (k: number) => number;
}

// Geometry for a whole displayed row as one fan. Overlap, per-card tilt and arc
// are all sized by the TOTAL number of displayed cards, so a 3-card row tucks
// into the same tight, angle-clamped arc a 16-card row would, instead of
// inheriting loose 3-card spacing and spilling off-screen with near-sideways
// edge cards.
export function fanGeometry(
  totalCards: number,
  cardWidthVar = "--hand-card-w",
  profile: FanGeometryProfile = "compact",
): FanGeometry {
  const center = (totalCards - 1) / 2;
  // Size the SHAPE (tilt + arc) from at least two cards so a lone card still
  // fans (a raw delta of 0 would render flat).
  const shape = Math.max(2, totalCards);
  const delta = profile === "wide"
    ? Math.min(4, 24 / (shape - 1))
    : Math.min(6, 36 / (shape - 1));
  const arcCoeff = getArcCoefficient(shape, profile);
  // Downward parabola (edges drop, center rides highest), clamped at the row's
  // own edges so the outermost cards rest level with the band instead of
  // sinking below it and clipping.
  const edgeLift = center * center * arcCoeff;
  return {
    overlap: getHandOverlap(totalCards, cardWidthVar, profile),
    rotation: (k: number) => (k - center) * delta,
    arc: (k: number) => {
      const d = k - center;
      return Math.min(d * d * arcCoeff, edgeLift);
    },
  };
}
