/** Recognizer for the "N-finger tap, repeated K times within a window" gesture. */

export const TRIPLE_TAP_WINDOW_MS = 500;
export const TRIPLE_TAP_FINGERS = 3;
export const TRIPLE_TAP_COUNT = 3;

export interface TripleTapState {
  count: number;
  lastTap: number;
}

export function initialTripleTapState(): TripleTapState {
  return { count: 0, lastTap: 0 };
}

export interface TripleTapOptions {
  fingers?: number;
  taps?: number;
  windowMs?: number;
}

/**
 * Advance the recognizer for a single `touchstart`.
 *
 * Only a touchstart that reaches exactly `fingers` simultaneous touches counts
 * as a tap. The 1- and 2-finger touchstarts that necessarily precede every
 * 3-finger gesture (fingers never land on the exact same millisecond) are
 * *ignored* — crucially, they must NOT reset progress, or the count can never
 * climb past 1 on real hardware. A tap more than `windowMs` after the previous
 * one starts a fresh sequence. Returns the next state and whether the full
 * gesture (`fingers`-finger tap performed `taps` times) just completed.
 */
export function advanceTripleTap(
  state: TripleTapState,
  touchCount: number,
  now: number,
  opts: TripleTapOptions = {},
): { state: TripleTapState; triggered: boolean } {
  const fingers = opts.fingers ?? TRIPLE_TAP_FINGERS;
  const taps = opts.taps ?? TRIPLE_TAP_COUNT;
  const windowMs = opts.windowMs ?? TRIPLE_TAP_WINDOW_MS;

  if (touchCount !== fingers) return { state, triggered: false };

  const withinWindow = now - state.lastTap <= windowMs;
  const count = (withinWindow ? state.count : 0) + 1;

  if (count >= taps) {
    return { state: { count: 0, lastTap: now }, triggered: true };
  }
  return { state: { count, lastTap: now }, triggered: false };
}
