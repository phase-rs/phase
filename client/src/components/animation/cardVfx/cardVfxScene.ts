// ─── Card VFX scene ───
// The shared WebGL overlay behind CardVfxLayer, loaded only through dynamic
// import so three.js stays out of the entry chunk. One renderer per layer
// mount, created by `initCardVfxScene` (the layer's first-effect init), never
// per effect: mobile browsers cap live WebGL contexts.
//
// Every entry point that can run long — init, the frame loop, and the
// re-init after a context restore — is defined here, so a long-animation-frame
// entry attributes it to this chunk.

import {
  MathUtils,
  type Object3D,
  PerspectiveCamera,
  Scene,
  SRGBColorSpace,
  Texture,
  WebGLRenderer,
} from "three";

import type { ManaColor, ObjectId } from "../../../adapter/types.ts";
import type { CardPose } from "./cardAnchors.ts";
import {
  type CardFlight,
  type CardFlightParams,
  cardFlightKind,
  createCardFlight,
  createPlaceholderTexture,
  restingState,
} from "./cardFlight.ts";
import { type CardShatterParams, cardShatterKind, createCardShatter } from "./cardShatter.ts";
import { createDamageStrike, type DamageHitParams, type DamageStrikeParams, damageStrikeKind } from "./damageStrike.ts";
import { createExileDissolve, type ExileDissolveParams, exileDissolveKind } from "./exileDissolve.ts";
import { createLandingDust, landingDustKind } from "./landingDust.ts";

/** One running effect. `update` draws a frame and returns whether it is still
 *  running; `dispose(silent)` frees it, and a silent dispose fires no
 *  completion callback (context loss, unmount, handoff). */
export interface SceneEffect {
  update(nowMs: number): boolean;
  dispose(silent: boolean): void;
}

/** What the scene lends its effects. */
export interface EffectHost {
  readonly scene: Scene;
  /** The shared card back, uploaded once in init; `null` when none loaded. */
  readonly backTexture: Texture | null;
  readonly placeholderTexture: Texture;
  /** The canvas rect, read once per frame: DOM rects minus its origin are
   *  canvas-local CSS px. */
  canvasOrigin(): DOMRectReadOnly;
}

/** A kind of scene effect. `warmUp` returns meshes built exactly as its
 *  effects build them (geometry, material, bound textures) at every tier, so
 *  init compiles every program an effect can use before any effect runs. */
export interface SceneEffectKind {
  warmUp(host: EffectHost): Object3D[];
}

export const SCENE_EFFECT_KINDS: readonly SceneEffectKind[] = [
  cardFlightKind,
  landingDustKind,
  cardShatterKind,
  exileDissolveKind,
  damageStrikeKind,
];

const CAMERA_FOV_DEG = 28;

/** Fits a perspective camera so the z = 0 plane maps 1:1 to CSS px, with
 *  world y = −(CSS y): a DOM rect places a mesh directly. */
export function fitPixelCamera(camera: PerspectiveCamera, w: number, h: number): void {
  const dist = h / 2 / Math.tan(MathUtils.degToRad(CAMERA_FOV_DEG / 2));
  camera.fov = CAMERA_FOV_DEG;
  camera.aspect = w / h;
  camera.position.set(w / 2, -h / 2, dist);
  camera.near = dist * 0.05;
  camera.far = dist * 4;
  camera.updateProjectionMatrix();
}

/** A card flight to start. `from` is the measured source; `null` continues the
 *  object's unreleased flight. `landingColors` tints the dust a battlefield
 *  landing kicks up at `full`; `null` is plain dust. */
export interface CardFlightRequest extends Omit<CardFlightParams, "from" | "back"> {
  from: CardPose | null;
  landingColors: readonly ManaColor[] | null;
}

/** The permanent a board effect happens to. */
interface OnPermanent {
  objectId: ObjectId;
}
/** A shatter to start; the scene supplies the pixel ratio. */
export type CardShatterRequest = Omit<CardShatterParams, "pixelRatio"> & OnPermanent;
/** A dissolve to start; the scene supplies the pixel ratio. */
export type ExileDissolveRequest = Omit<ExileDissolveParams, "pixelRatio"> & OnPermanent;
/** A damage strike to start; a hit names the permanent it lands on. */
export type DamageStrikeRequest = Omit<DamageStrikeParams, "hit"> & { hit: (DamageHitParams & OnPermanent) | null };

export interface CardVfxScene {
  setPixelRatio(ratio: number): void;
  /** Builds a texture and uploads it now, so no effect frame uploads. */
  uploadFace(image: HTMLImageElement | HTMLCanvasElement): Texture;
  hasBack(): boolean;
  hasFlight(objectId: ObjectId): boolean;
  /** Starts a flight, handing off from the object's unreleased flight if it has
   *  one; a released (revealing or fading) flight is disposed and the new one
   *  starts from the source. Returns false when there is neither a source nor an
   *  unreleased flight. */
  startCardFlight(request: CardFlightRequest): boolean;
  /** Silently disposes the object's flight if it has released (is revealing or
   *  fading). A later presentation of the object supersedes its landing. */
  dropReleasedFlight(objectId: ObjectId): void;
  /** Board effects — a shatter, a dissolve, a strike's hit — happen to one
   *  permanent at a time: each silently disposes the permanent's running one,
   *  whose veil the new effect's completion then releases. */
  startShatter(request: CardShatterRequest): void;
  startDissolve(request: ExileDissolveRequest): void;
  startDamageStrike(request: DamageStrikeRequest): void;
  add(effect: SceneEffect): void;
  dispose(): void;
}

export interface CardVfxSceneCallbacks {
  /** The card back, already settled; `null` when no source loaded. */
  backImage: HTMLImageElement | null;
  /** The scene is warm: every program compiled and drawn once. */
  onReady(scene: CardVfxScene): void;
  /** Renderer creation threw; the overlay is unusable. */
  onFailed(): void;
  /** The context was lost; every running effect has been dropped silently. */
  onContextLost(): void;
  /** The context is back; `onReady` follows once the scene is warm again. */
  onContextRestored(): void;
}

function forget(effects: Map<ObjectId, SceneEffect>, effect: SceneEffect) {
  for (const [objectId, running] of effects) {
    if (running === effect) effects.delete(objectId);
  }
}

function uploadableTexture(image: HTMLImageElement | HTMLCanvasElement): Texture {
  const texture = new Texture(image);
  texture.colorSpace = SRGBColorSpace;
  texture.anisotropy = 4;
  texture.needsUpdate = true;
  return texture;
}

class CardVfxSceneRuntime implements CardVfxScene, EffectHost {
  readonly scene = new Scene();
  readonly placeholderTexture = createPlaceholderTexture();
  readonly backTexture: Texture | null;
  private readonly camera = new PerspectiveCamera(CAMERA_FOV_DEG, 1, 1, 10000);
  private readonly active = new Set<SceneEffect>();
  private readonly flights = new Map<ObjectId, CardFlight>();
  private readonly boardEffects = new Map<ObjectId, SceneEffect>();
  private readonly resizeObserver: ResizeObserver;
  private warmUpObjects: Object3D[] | null = null;
  private pixelRatio = 1;
  private origin: DOMRectReadOnly;
  private raf = 0;
  private sizeDirty = true;
  private contextLost = false;

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly renderer: WebGLRenderer,
    private readonly callbacks: CardVfxSceneCallbacks,
  ) {
    renderer.setClearColor(0x000000, 0);
    this.backTexture = callbacks.backImage ? uploadableTexture(callbacks.backImage) : null;
    this.origin = canvas.getBoundingClientRect();
    this.resizeObserver = new ResizeObserver(() => {
      this.sizeDirty = true;
    });
    this.resizeObserver.observe(canvas);
    canvas.addEventListener("webglcontextlost", this.handleContextLost);
    canvas.addEventListener("webglcontextrestored", this.handleContextRestored);
  }

  /** Warms the scene, then reports ready unless the context was lost meanwhile. */
  async start(): Promise<void> {
    if (await this.prepare()) this.callbacks.onReady(this);
  }

  // Compiles every registered effect's programs and draws them once on the
  // hidden canvas, so link cost never lands in an effect's frame. The warm-up
  // meshes are kept: three frees a program once no material uses it, and the
  // next effect would compile it again. They draw only during this frame.
  private async prepare(): Promise<boolean> {
    this.applySize();
    if (!this.warmUpObjects) {
      this.warmUpObjects = SCENE_EFFECT_KINDS.flatMap((kind) => kind.warmUp(this));
      this.scene.add(...this.warmUpObjects);
    }
    for (const object of this.warmUpObjects) {
      object.visible = true;
      object.frustumCulled = false;
    }
    if (this.backTexture) this.renderer.initTexture(this.backTexture);
    await this.renderer.compileAsync(this.scene, this.camera);
    this.renderer.render(this.scene, this.camera);
    for (const object of this.warmUpObjects) object.visible = false;
    return !this.contextLost;
  }

  private readonly handleContextLost = () => {
    this.contextLost = true;
    cancelAnimationFrame(this.raf);
    this.raf = 0;
    this.disposeEffects();
    this.canvas.style.visibility = "hidden";
    this.callbacks.onContextLost();
  };

  // three re-creates its GL state in its own listener, registered first, so
  // the renderer is usable again here: re-warm it rather than make another.
  private readonly handleContextRestored = () => {
    this.contextLost = false;
    this.callbacks.onContextRestored();
    void this.start();
  };

  private readonly frame = (nowMs: number) => {
    this.raf = 0;
    if (this.sizeDirty) this.applySize();
    this.origin = this.canvas.getBoundingClientRect();
    for (const effect of this.active) {
      if (effect.update(nowMs)) continue;
      this.active.delete(effect);
      effect.dispose(false);
      forget(this.flights, effect);
      forget(this.boardEffects, effect);
    }
    this.renderer.render(this.scene, this.camera);
    // Render only while something runs: an idle overlay requests no frame.
    const running = this.active.size > 0;
    this.canvas.style.visibility = running ? "visible" : "hidden";
    if (running) this.raf = requestAnimationFrame(this.frame);
  };

  private applySize() {
    const { width, height } = this.canvas.getBoundingClientRect();
    const w = Math.max(1, width);
    const h = Math.max(1, height);
    this.renderer.setSize(w, h, false);
    fitPixelCamera(this.camera, w, h);
    this.sizeDirty = false;
  }

  private disposeEffects() {
    for (const effect of this.active) effect.dispose(true);
    this.active.clear();
    this.flights.clear();
    this.boardEffects.clear();
  }

  canvasOrigin(): DOMRectReadOnly {
    return this.origin;
  }

  setPixelRatio(ratio: number) {
    this.pixelRatio = ratio;
    this.renderer.setPixelRatio(ratio);
  }

  uploadFace(image: HTMLImageElement | HTMLCanvasElement): Texture {
    const texture = uploadableTexture(image);
    this.renderer.initTexture(texture);
    return texture;
  }

  hasBack() {
    return this.backTexture !== null;
  }

  hasFlight(objectId: ObjectId) {
    return this.flights.get(objectId)?.isReleased() === false;
  }

  dropReleasedFlight(objectId: ObjectId) {
    const flight = this.flights.get(objectId);
    if (!flight?.isReleased()) return;
    this.active.delete(flight);
    // Already released and unveiled: nothing to complete.
    flight.dispose(true);
    this.flights.delete(objectId);
  }

  startCardFlight(request: CardFlightRequest): boolean {
    const { objectId } = request;
    const previous = this.flights.get(objectId);
    // An unreleased flight hands off from where its card is; a released one
    // (revealing or fading) is over, so the new flight starts from the source.
    const from =
      previous && !previous.isReleased()
        ? previous.currentState()
        : request.from && restingState(request.from, request.flip);
    if (!from) return false;
    if (previous) {
      // One object, one flight.
      this.active.delete(previous);
      previous.dispose(true);
    }
    const { landingColors, ...params } = request;
    const flight = createCardFlight(this, {
      ...params,
      from,
      back: this.backTexture,
      onRelease: (reason) => {
        if (reason === "land" && request.route.to === "Battlefield" && request.tier === "full") {
          this.add(
            createLandingDust(this, flight.currentState(), landingColors, request.pace, this.pixelRatio),
          );
        }
        request.onRelease(reason);
      },
    });
    this.flights.set(objectId, flight);
    this.add(flight);
    return true;
  }

  startShatter({ objectId, ...request }: CardShatterRequest) {
    this.addBoardEffect(objectId, createCardShatter(this, { ...request, pixelRatio: this.pixelRatio }));
  }

  startDissolve({ objectId, ...request }: ExileDissolveRequest) {
    this.addBoardEffect(objectId, createExileDissolve(this, { ...request, pixelRatio: this.pixelRatio }));
  }

  startDamageStrike(request: DamageStrikeRequest) {
    const { strike, hit } = createDamageStrike(this, request);
    this.add(strike);
    // A hit taking over from a running effect inherits its veil, so it shows
    // the permanent at rest until its own impact.
    if (hit && request.hit && this.addBoardEffect(request.hit.objectId, hit)) hit.showAtRest();
  }

  /** Returns whether `effect` took over from one already running. */
  private addBoardEffect(objectId: ObjectId, effect: SceneEffect): boolean {
    const previous = this.boardEffects.get(objectId);
    if (previous) {
      this.active.delete(previous);
      previous.dispose(true);
    }
    this.boardEffects.set(objectId, effect);
    this.add(effect);
    return previous !== undefined;
  }

  add(effect: SceneEffect) {
    this.active.add(effect);
    if (!this.raf && !this.contextLost) this.raf = requestAnimationFrame(this.frame);
  }

  dispose() {
    // Detach first: forceContextLoss dispatches `webglcontextlost` on the
    // canvas, which must not run the loss path after unmount.
    this.canvas.removeEventListener("webglcontextlost", this.handleContextLost);
    this.canvas.removeEventListener("webglcontextrestored", this.handleContextRestored);
    this.resizeObserver.disconnect();
    cancelAnimationFrame(this.raf);
    this.raf = 0;
    this.disposeEffects();
    this.renderer.dispose();
    this.renderer.forceContextLoss();
  }
}

/** The layer's first-effect init, posted as a task after a painted frame.
 *  Creates the one renderer, then warms it; a creation failure is final. */
export function initCardVfxScene(canvas: HTMLCanvasElement, callbacks: CardVfxSceneCallbacks): void {
  let renderer: WebGLRenderer;
  try {
    renderer = new WebGLRenderer({ canvas, alpha: true, antialias: false, powerPreference: "low-power" });
  } catch {
    callbacks.onFailed();
    return;
  }
  void new CardVfxSceneRuntime(canvas, renderer, callbacks).start();
}
