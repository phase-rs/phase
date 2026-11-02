import type { ManaColor } from "../../adapter/types.ts";

export const AMBIANCE_GLSL = /* glsl */ `
  float rain(vec2 uv) {
    vec2 p = uv * vec2(110.0, 45.0);
    p.x += p.y * 0.22;
    float lane = floor(p.x);
    p.y += uTime * (9.0 + hash21(vec2(lane, 3.0)) * 6.0);
    vec2 cell = floor(p);
    vec2 f = fract(p);
    float dropMask = step(0.96, hash21(cell));
    return dropMask * (1.0 - smoothstep(0.015, 0.09, abs(f.x - 0.5)))
      * smoothstep(0.1, 0.5, f.y) * (1.0 - smoothstep(0.5, 0.95, f.y));
  }

  float motes(vec2 uv, float speed, float radius) {
    float light = 0.0;
    for (int i = 0; i < 40; i++) {
      float seed = float(i);
      vec2 start = vec2(hash21(vec2(seed, 1.0)), hash21(vec2(seed, 8.0)));
      vec2 pos = fract(start + vec2(sin(uTime * 0.4 + seed) * 0.025, uTime * speed));
      vec2 d = (uv - pos) * vec2(1.778, 1.0);
      light += exp(-dot(d, d) / (radius * radius)) * (0.5 + 0.5 * sin(seed + uTime));
    }
    return light;
  }
`;

// Each painting gets its own material motion; weather stays outside the playing floor.
export const ARENA_EFFECTS: Record<Exclude<ManaColor, "Blue">, string> = {
  White: /* glsl */ `
    float clouds = vnoise(uv * 9.0 + vec2(uTime * 0.07, uTime * 0.025));
    color += vec3(0.13, 0.11, 0.07) * clouds * rim * uIntensity;
    color += vec3(0.8, 0.67, 0.36) * motes(uv, 0.012, 0.003) * rim * uAmbiance;
  `,
  Black: /* glsl */ `
    float fog = vnoise(uv * 7.0 + vec2(uTime * 0.07, -uTime * 0.04));
    float detail = vnoise(uv * 19.0 + vec2(-uTime * 0.03, uTime * 0.02));
    color = mix(color, vec3(0.20, 0.23, 0.26), fog * detail * rim * 0.65 * uAmbiance);
    float candle = smoothstep(0.08, 0.25, original.r - original.b);
    color += vec3(0.16, 0.065, 0.015) * candle * rim
      * vnoise(uv * 60.0 + uTime * 3.0) * uIntensity;
  `,
  Red: /* glsl */ `
    float lava = smoothstep(0.10, 0.32, original.r - original.g) * rim;
    float flow = vnoise(uv * 28.0 + vec2(uTime * 0.18, -uTime * 0.10));
    vec2 heat = vec2(sin(uv.y * 90.0 + uTime * 2.0), cos(uv.x * 70.0 - uTime)) * 0.003;
    color = texture2D(uArt, uv + heat * lava * uIntensity).rgb;
    color += vec3(0.3, 0.055, 0.005) * lava * flow * uIntensity;
    color += vec3(0.95, 0.23, 0.025) * motes(uv, 0.06, 0.002) * rim * uAmbiance;
  `,
  Green: /* glsl */ `
    float foliage = smoothstep(0.015, 0.08, original.g - original.r) * rim;
    vec2 breeze = vec2(sin(uv.y * 35.0 + uTime * 0.8), cos(uv.x * 28.0 - uTime * 0.6));
    color = texture2D(uArt, uv + breeze * foliage * 0.002 * uIntensity).rgb;
    color *= 1.0 - vnoise(uv * 12.0 + vec2(uTime * 0.06, 0.0)) * rim * 0.13 * uIntensity;
    color += vec3(0.72, 0.80, 0.85) * motes(uv, -0.035, 0.0035) * rim * uAmbiance;
  `,
};
