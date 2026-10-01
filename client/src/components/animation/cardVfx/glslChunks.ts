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

/** The front traverses the noisy burn key over this span. */
export const BURN_SPAN = 1.55;

/** `burnKey`: arrival of fire at a card pixel; callers provide `fbm`. */
export const BURN_KEY_GLSL = /* glsl */ `
  uniform vec2 uSize, uKeyImpact; uniform float uKeyMax, uKeyMode;
  float burnKey(vec2 c) {
    float base;
    if (uKeyMode < 0.5) base = distance(c, uKeyImpact) / uKeyMax;
    else { vec2 e = min(c, uSize - c); base = clamp(min(e.x, e.y) / (0.5 * min(uSize.x, uSize.y)), 0.0, 1.0); }
    return base * 0.8 + (fbm(c * 0.04) - 0.5) * 0.4 + 0.15;
  }`;

/** `scorchMask`: the charred patch a fire strike and its later burn share. */
export const SCORCH_GLSL = /* glsl */ `
  uniform vec2 uImpact; uniform float uScorchR;
  float scorchMask(vec2 c) {
    float r = distance(c, uImpact) / uScorchR;
    return 1.0 - smoothstep(0.3, 1.0, r + (fbm(c * 0.09 + 7.0) - 0.5) * 0.7);
  }`;
