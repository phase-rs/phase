import type { CappedTrack } from "../../stores/preferencesStore.ts";

/** Per-band clamps so a resize can never collapse a zone to nothing or starve
 *  the battlefield (the `1fr` middle). A band may not exceed `MAX_FRACTION` of
 *  the viewport height. */
const MIN_PCT = 6;
const MAX_PCT = 35;
const MIN_PX = 40;
const MAX_FRACTION = 0.4;
/** Within this many px of the default band height, a resize snaps back to it. */
const SNAP_BAND_PX = 12;

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

/**
 * Pure resize math: given a band's current {@link CappedTrack}, a pixel delta
 * (positive = grow the band), and the viewport height, return the new track.
 * Both `pct` and `pxCap` are set to agree at the current viewport so the band
 * renders at exactly the dragged height now and still scales with the window.
 */
export function resizeBand(
  current: CappedTrack,
  deltaPx: number,
  viewportH: number,
  snapToDefault?: CappedTrack,
): CappedTrack {
  // Degenerate viewport (zero / negative / NaN height — window.innerHeight can be
  // 0 transiently for a collapsed/pre-layout container): no meaningful resize is
  // possible, and the pct math below would divide by it and yield a NaN (which
  // persists as null) or a negative pxCap. Return the current track unchanged,
  // mirroring ratioFromPointerX's degenerate-region guard.
  if (!(viewportH > 0)) return current;
  const currentPx = Math.min((current.pct / 100) * viewportH, current.pxCap);
  const maxPx = viewportH * MAX_FRACTION;
  const nextPx = clamp(currentPx + deltaPx, MIN_PX, maxPx);
  // Magnetic snap to the default band height (its effective px at this viewport),
  // returning the default track verbatim so the home value is restored exactly.
  if (snapToDefault) {
    const defaultPx = Math.min((snapToDefault.pct / 100) * viewportH, snapToDefault.pxCap);
    if (Math.abs(nextPx - defaultPx) < SNAP_BAND_PX) return { ...snapToDefault };
  }
  const pct = clamp((nextPx / viewportH) * 100, MIN_PCT, MAX_PCT);
  return { pct: Math.round(pct * 10) / 10, pxCap: Math.round(nextPx) };
}

/**
 * Pure lands↔support split math: given the absolute pointer X and the left/right
 * viewport bounds of the combined two-column region, return lands' share (0..1).
 * Drift-free — derived from the pointer's absolute position rather than an
 * accumulated delta. The store applies the safety clamp; a degenerate region
 * (right ≤ left) falls back to an even split.
 */
export function ratioFromPointerX(pointerX: number, left: number, right: number): number {
  if (right <= left) return 0.5;
  return (pointerX - left) / (right - left);
}
