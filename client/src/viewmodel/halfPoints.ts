import type { HalfFraction } from "../adapter/types";

/**
 * Wire codec for the engine's Unhinged half-point values. The engine sends a
 * half-point number as a whole part plus a `HalfFraction` remainder (2½ is
 * `2` + `"Half"`); these helpers convert that pair to and from a count of
 * halves so a prompt can step and submit exact values. Legality stays with the
 * engine.
 */

/** The number of halves in `whole` + `fraction` (`2` + `"Half"` is 5). */
export function toHalves(whole: number, fraction?: HalfFraction): number {
  return whole * 2 + (fraction === "Half" ? 1 : 0);
}

/** Split a count of halves back into the engine's whole + fraction pair. */
export function fromHalves(halves: number): { whole: number; fraction: HalfFraction } {
  return { whole: Math.floor(halves / 2), fraction: halves % 2 === 0 ? "Zero" : "Half" };
}

/** The numeric value of `whole` + `fraction` (`2` + `"Half"` is 2.5). */
export function halfPointValue(whole: number, fraction?: HalfFraction): number {
  return toHalves(whole, fraction) / 2;
}

/** Display a count of halves as the printed number: 5 → "2½", 1 → "½", 4 → "2". */
export function formatHalves(halves: number): string {
  const whole = Math.floor(Math.abs(halves) / 2);
  const sign = halves < 0 ? "-" : "";
  if (halves % 2 === 0) return `${sign}${whole}`;
  return whole === 0 ? `${sign}½` : `${sign}${whole}½`;
}
