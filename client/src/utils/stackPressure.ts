/**
 * Display-only stack pacing. Mirrors `crates/engine/src/game/stack.rs`'s
 * `StackPressure` + named thresholds — the engine is the authoritative
 * source; this mirror exists because reading the WASM-exposed
 * `get_stack_pressure()` on every frame would round-trip needlessly for a
 * value derived from `state.stack.length`, which is already in the frontend.
 * Keep this file in lockstep with the engine — the Rust-side unit tests
 * enforce the boundary semantics.
 */

export type StackPressure = "Normal" | "Elevated" | "Rapid" | "Instant";

export const STACK_PRESSURE_ELEVATED = 10;
export const STACK_PRESSURE_RAPID = 30;
export const STACK_PRESSURE_INSTANT = 100;

export function stackPressureFromLength(length: number): StackPressure {
  if (length >= STACK_PRESSURE_INSTANT) return "Instant";
  if (length >= STACK_PRESSURE_RAPID) return "Rapid";
  if (length >= STACK_PRESSURE_ELEVATED) return "Elevated";
  return "Normal";
}

/**
 * Per-entry animation delay/duration multiplier. At `Instant`, the mount
 * animation is effectively skipped; at `Normal` the standard timing applies.
 */
export function pressureMultiplier(pressure: StackPressure): number {
  switch (pressure) {
    case "Normal":
      return 1.0;
    case "Elevated":
      return 0.5;
    case "Rapid":
      return 0.15;
    case "Instant":
      return 0;
  }
}
