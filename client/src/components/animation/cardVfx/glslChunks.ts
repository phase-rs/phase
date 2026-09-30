// ─── Shared GLSL ───
// Shader functions more than one card VFX uses, spliced into their sources.

/** `roundedBox`: signed distance from `p` to a box of half-size `b` with corner
 *  radius `r`, negative inside. */
export const ROUNDED_BOX_GLSL = /* glsl */ `
  float roundedBox(vec2 p, vec2 b, float r) { vec2 q = abs(p) - b + r; return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r; }`;

/** `cornerMask`: 1 inside a card of `size` (card px, origin top-left) with
 *  rounded corners, 0 outside, antialiased over one pixel. */
export const CORNER_MASK_GLSL = /* glsl */ `
  ${ROUNDED_BOX_GLSL}
  float cornerMask(vec2 card, vec2 size, float radius) { return clamp(0.5 - roundedBox(card - size * 0.5, size * 0.5, radius), 0.0, 1.0); }`;

/** `hash21`, a per-cell random value, and `vnoise`, smooth value noise in [0, 1]. */
export const VALUE_NOISE_GLSL = /* glsl */ `
  float hash21(vec2 p) { p = fract(p * vec2(123.34, 456.21)); p += dot(p, p + 45.32); return fract(p.x * p.y); }
  float vnoise(vec2 p) {
    vec2 i = floor(p), f = fract(p), u = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash21(i), hash21(i + vec2(1.0, 0.0)), u.x), mix(hash21(i + vec2(0.0, 1.0)), hash21(i + vec2(1.0, 1.0)), u.x), u.y);
  }`;
