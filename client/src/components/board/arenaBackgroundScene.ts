import {
  Mesh,
  OrthographicCamera,
  PlaneGeometry,
  Scene,
  ShaderMaterial,
  SRGBColorSpace,
  TextureLoader,
  Vector2,
  WebGLRenderer,
} from "three";

import type { ManaColor } from "../../adapter/types.ts";
import { ARENA_ART } from "./arenaBackgrounds.ts";
import { ARENA_EFFECTS, AMBIANCE_GLSL } from "./arenaBackgroundShaders.ts";

import type { VfxQuality } from "../../animation/types.ts";
import { VALUE_NOISE_GLSL } from "../animation/cardVfx/glslChunks.ts";

const vertexShader = /* glsl */ `
  varying vec2 vUv;
  void main() {
    vUv = uv;
    gl_Position = vec4(position.xy, 0.0, 1.0);
  }
`;

function fragmentShader(color: ManaColor): string {
  return /* glsl */ `
    uniform sampler2D uArt;
    uniform vec2 uCover;
    uniform float uTime, uIntensity, uAmbiance;
    varying vec2 vUv;
    ${VALUE_NOISE_GLSL}
    ${AMBIANCE_GLSL}
    ${color === "Blue" ? BLUE_FLAME : ""}
    void main() {
      vec2 uv = (vUv - 0.5) * uCover + 0.5;
      vec3 original = texture2D(uArt, uv).rgb;
      float edge = max(abs(uv.x - 0.5) / 0.5, abs(uv.y - 0.5) / 0.5);
      float rim = smoothstep(0.68, 0.94, edge);
      vec3 color = original;
      ${color === "Blue" ? BLUE_EFFECT : ARENA_EFFECTS[color]}
      gl_FragColor = vec4(color, 1.0);
      #include <colorspace_fragment>
    }
  `;
}

const BLUE_FLAME = /* glsl */ `  float flame(vec2 uv, vec2 center, float seed) {
    vec2 p = (uv - center) / vec2(0.017, 0.038);
    float shape = exp(-dot(p, p) * 2.0);
    float flutter = vnoise(vec2(seed, uTime * 3.0));
    return shape * (0.25 + flutter * 0.75);
  }

`;
const BLUE_EFFECT = /* glsl */ `

    // Art-directed prototype mask: only blue/cyan pixels at the outer shoreline.
    // The quiet playing floor is never displaced, including at narrow viewports.

    float blue = smoothstep(0.025, 0.11, original.b - original.r);
    float cyan = 1.0 - smoothstep(0.08, 0.22, original.b - original.g);
    float water = rim * blue * cyan;
    float drift = vnoise(uv * vec2(30.0, 18.0) + vec2(uTime * 0.10, -uTime * 0.07));
    vec2 ripple = vec2(
      sin(uv.y * 95.0 + uTime * 2.2 + drift * 4.0),
      cos(uv.x * 80.0 - uTime * 1.6 + drift * 3.0)
    );
    color = texture2D(uArt, uv + ripple * water * 0.006 * uIntensity).rgb;
    float glint = pow(max(0.0, sin(uv.y * 210.0 + uv.x * 55.0 - uTime * 0.7 + drift * 5.0)), 12.0);
    color += vec3(0.07, 0.13, 0.15) * water * glint * uIntensity;
    // UV anchors belong to this painting, so CSS-cover cropping moves them with it.
    float fire = flame(uv, vec2(0.066, 0.907), 1.0)
               + flame(uv, vec2(0.933, 0.907), 4.0)
               + flame(uv, vec2(0.039, 0.267), 7.0)
               + flame(uv, vec2(0.957, 0.261), 10.0);
    color += vec3(0.04, 0.15, 0.22) * fire * uIntensity;
    color += vec3(0.06, 0.09, 0.12) * rain(uv) * smoothstep(0.84, 0.98, edge) * uAmbiance;
`;



/** A single image-plane pass, independent of card VFX's foreground/veil lifecycle. */
export function createArenaBackgroundScene(
  canvas: HTMLCanvasElement,
  color: ManaColor,
  quality: Exclude<VfxQuality, "minimal">,
  intensity: number,
  ambiance: number,
): () => void {
  let renderer: WebGLRenderer;
  try {
    renderer = new WebGLRenderer({ canvas, alpha: true, antialias: false, powerPreference: "low-power" });
  } catch {
    return () => {};
  }
  renderer.setPixelRatio(Math.min(window.devicePixelRatio, quality === "full" ? 1.5 : 1));
  const scene = new Scene();
  const camera = new OrthographicCamera(-1, 1, 1, -1, 0, 1);
  const cover = new Vector2(1, 1);
  const material = new ShaderMaterial({
    vertexShader,
    fragmentShader: fragmentShader(color),
    depthTest: false,
    depthWrite: false,
    uniforms: {
      uArt: { value: null },
      uCover: { value: cover },
      uTime: { value: 0 },
      uIntensity: { value: intensity },
      uAmbiance: { value: ambiance },
    },
  });
  const geometry = new PlaneGeometry(2, 2);
  scene.add(new Mesh(geometry, material));
  let disposed = false;
  let ready = false;
  let lost = false;
  let raf = 0;
  let previous = 0;
  let elapsed = 0;
  let sizeDirty = true;
  const interval = 1000 / (quality === "full" ? 30 : 20);
  const observer = new ResizeObserver(() => { sizeDirty = true; });
  observer.observe(canvas);

  const texture = new TextureLoader().load(ARENA_ART[color], (loaded) => {
    if (disposed) { loaded.dispose(); return; }
    loaded.colorSpace = SRGBColorSpace;
    material.uniforms.uArt.value = loaded;
    ready = true;
    resume();
  });

  function frame(now: number) {
    raf = 0;
    if (disposed || lost || document.hidden || !ready) return;
    raf = requestAnimationFrame(frame);
    if (previous && now - previous < interval) return;
    if (sizeDirty) {
      const { width, height } = canvas.getBoundingClientRect();
      renderer.setSize(Math.max(1, width), Math.max(1, height), false);
      const image = texture.image as HTMLImageElement;
      const aspect = Math.max(1, width) / Math.max(1, height);
      const imageAspect = image.width / image.height;
      cover.set(Math.min(1, aspect / imageAspect), Math.min(1, imageAspect / aspect));
      sizeDirty = false;
    }
    if (previous) elapsed += Math.min(now - previous, 100);
    previous = now;
    material.uniforms.uTime.value = elapsed / 1000;
    renderer.render(scene, camera);
    canvas.style.visibility = "visible";
  }

  function resume() {
    cancelAnimationFrame(raf);
    raf = 0;
    previous = 0;
    if (!disposed && !lost && ready && !document.hidden) raf = requestAnimationFrame(frame);
  }
  function onLost() {
    lost = true;
    cancelAnimationFrame(raf);
    canvas.style.visibility = "hidden";
  }
  function onRestored() { lost = false; sizeDirty = true; resume(); }
  document.addEventListener("visibilitychange", resume);
  canvas.addEventListener("webglcontextlost", onLost);
  canvas.addEventListener("webglcontextrestored", onRestored);

  return () => {
    disposed = true;
    cancelAnimationFrame(raf);
    document.removeEventListener("visibilitychange", resume);
    canvas.removeEventListener("webglcontextlost", onLost);
    canvas.removeEventListener("webglcontextrestored", onRestored);
    observer.disconnect();
    canvas.style.visibility = "hidden";
    geometry.dispose();
    material.dispose();
    texture.dispose();
    renderer.dispose();
    renderer.forceContextLoss();
  };
}
