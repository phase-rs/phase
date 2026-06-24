/**
 * Display-only stack *throughput* pacing — the rate companion to the
 * depth-based `StackPressure` mirror in `stackPressure.ts`.
 *
 * The engine's `StackPressure` reads instantaneous stack *depth*. That signal is
 * blind to low-depth-high-churn loops (e.g. Exquisite Blood + Sanguine Bond,
 * which oscillates the stack between 0 and 1): depth never crosses the Elevated
 * threshold, so every cycle pays full artificial wait even though dozens of
 * items are flowing through. This module adds the missing *rate* axis — a
 * sliding window of recent stack resolutions — and combines it with depth via
 * `effectiveStackPressure`, so pacing escalates when EITHER the stack is deep OR
 * it is churning fast.
 *
 * This is wall-clock-derived and therefore strictly frontend-only: the engine is
 * a pure, clock-free reducer and cannot (and must not) express "resolutions per
 * second". Pacing is a presentation choice, the same category as
 * `animationSpeedMultiplier`.
 */
import { type StackPressure, stackPressureFromLength } from "./stackPressure";

// Snappy defaults — tune alongside the engine-mirrored STACK_PRESSURE_* depth
// thresholds. A 0↔1 loop reaches Rapid (0.15× pacing) within ~1s of sustained
// churn, then restores full pacing ~THROUGHPUT_WINDOW_MS after the churn stops
// (the window self-empties — no explicit reset needed for the common case).
export const THROUGHPUT_WINDOW_MS = 1500;
export const RATE_PRESSURE_ELEVATED = 8;
export const RATE_PRESSURE_RAPID = 24;

// Monotonic non-decreasing ring of resolution timestamps (performance.now()).
// Bounded by RATE_PRESSURE_RAPID-ish counts within the window in practice; a
// pathological burst is pruned to the window on every record/read.
const resolutionTimes: number[] = [];

function prune(now: number): void {
  const cutoff = now - THROUGHPUT_WINDOW_MS;
  let drop = 0;
  while (drop < resolutionTimes.length && resolutionTimes[drop] < cutoff) drop++;
  if (drop > 0) resolutionTimes.splice(0, drop);
}

/** Record `count` stack items that just left the stack (resolved/countered). */
export function recordStackResolutions(
  count: number,
  now: number = performance.now(),
): void {
  for (let i = 0; i < count; i++) resolutionTimes.push(now);
  prune(now);
}

/** Clear throughput history — for new-game boundaries and tests. */
export function resetStackThroughput(): void {
  resolutionTimes.length = 0;
}

/**
 * Pressure implied purely by recent resolution *rate*. Capped at `Rapid`:
 * `Instant` (skip animation entirely) stays a depth-only verdict — a true
 * hundreds-deep storm — because mere fast oscillation still wants a visible
 * flipbook of life/counter changes, not a blank skip.
 */
export function stackPressureFromRate(now: number = performance.now()): StackPressure {
  prune(now);
  const n = resolutionTimes.length;
  if (n >= RATE_PRESSURE_RAPID) return "Rapid";
  if (n >= RATE_PRESSURE_ELEVATED) return "Elevated";
  return "Normal";
}

const PRESSURE_RANK: Record<StackPressure, number> = {
  Normal: 0,
  Elevated: 1,
  Rapid: 2,
  Instant: 3,
};

/**
 * The pacing pressure every display / auto-pass consumer should read: the hotter
 * of depth (`stackPressureFromLength`) and recent rate (`stackPressureFromRate`).
 * `max` because either a deep stack or fast churn warrants speeding up; pacing
 * relaxes only when both signals are quiet.
 */
export function effectiveStackPressure(
  stackLen: number,
  now: number = performance.now(),
): StackPressure {
  const byDepth = stackPressureFromLength(stackLen);
  const byRate = stackPressureFromRate(now);
  return PRESSURE_RANK[byRate] > PRESSURE_RANK[byDepth] ? byRate : byDepth;
}
