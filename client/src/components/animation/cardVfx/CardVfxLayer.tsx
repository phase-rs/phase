// ─── Card VFX layer ───
// The React side of the shared WebGL overlay: the canvas, the hidden image
// loaders, and the `present` handle through which AnimationOverlay offers each
// flight-eligible event. Every `present` ends in exactly one presentation —
// a GL flight, or the Classic effect it was handed — and the animation queue
// never waits on either.
//
// Only types come from three.js and the scene here; the scene module is
// fetched by dynamic import on mount, so three stays out of the entry chunk.

import {
  type CSSProperties,
  forwardRef,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import type { Texture } from "three";

import type { ObjectId } from "../../../adapter/types.ts";
import { useCardBackImage } from "../../../hooks/useCardImage.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { type AnimationImageSnapshot, ResolvedAnimationImage } from "../ResolvedAnimationImage.tsx";
import { type Aim, type CardPose, measureCardPose, resolveAim, sourceElement } from "./cardAnchors.ts";
import type { CardVfxTier, FlightFlip } from "./cardFlight.ts";
import type { CardFlightRoute, CardFlightSpec } from "./cardFlightSpecs.ts";
import type { CardVfxScene, CardVfxSceneCallbacks } from "./cardVfxScene.ts";
import type * as CardVfxSceneModule from "./cardVfxScene.ts";

/** A flight whose face has not loaded by this deadline presents Classic. */
export const CARD_FLIGHT_FACE_READY_MAX_MS = 150;
/** The face deadline never exceeds this fraction of the owning step. */
const FACE_READY_STEP_FRACTION = 0.3;
/** Device-pixel-ratio caps: `full` by pointer, `reduced` under either. */
export const PIXEL_RATIO_CAP = { fine: 2, coarse: 1.5, reduced: 1 } as const;

/** Whether the overlay can run at all. three.js requires WebGL 2; checking
 *  the API creates no context, so the check costs no context slot. */
export function cardVfxSupported(): boolean {
  return typeof WebGL2RenderingContext === "function";
}

export interface CardVfxLayerHandle {
  /** Presents `spec` as a GL flight, or runs `classic` instead. Exactly one
   *  of the two happens, once. */
  present(spec: CardFlightSpec, classic: () => void): void;
}

interface CardVfxLayerProps {
  tier: CardVfxTier;
}

type LayerState = "idle" | "initializing" | "ready" | "lost" | "failed";

/** A flight waiting for its face image. Later presents for the same object
 *  join it: one flight from the first source to the last destination. */
interface PendingStart {
  token: number;
  spec: CardFlightSpec;
  from: CardPose | null;
  face: AnimationImageSnapshot;
  classics: (() => void)[];
  deadline: ReturnType<typeof setTimeout>;
}

function flipFor({ startFace, endFace }: CardFlightSpec): FlightFlip {
  if (!startFace && endFace) return "toFront";
  if (startFace && !endFace) return "toBack";
  return "none";
}

function faceKey(face: AnimationImageSnapshot): string {
  return JSON.stringify([face.cardName, face.faceIndex, face.oracleId, face.faceName, face.isToken]);
}

function pixelRatioFor(tier: CardVfxTier): number {
  switch (tier) {
    case "full": {
      const coarse = window.matchMedia("(pointer: coarse)").matches;
      return Math.min(window.devicePixelRatio, coarse ? PIXEL_RATIO_CAP.coarse : PIXEL_RATIO_CAP.fine);
    }
    case "reduced":
      return Math.min(window.devicePixelRatio, PIXEL_RATIO_CAP.reduced);
  }
}

function runAll(classics: readonly (() => void)[]) {
  for (const classic of classics) classic();
}

class CardVfxController {
  private state: LayerState = "idle";
  private lifecycle = { disposed: true };
  private canvas: HTMLCanvasElement | null = null;
  private sceneModule: typeof CardVfxSceneModule | null = null;
  /** `undefined` until the back loader settles; `null` when no back loaded. */
  private backImage: HTMLImageElement | null | undefined = undefined;
  private initScheduled = false;
  private initFrame = 0;
  private initTask: ReturnType<typeof setTimeout> | undefined;
  private scene: CardVfxScene | null = null;
  private lastStackPose: CardPose | null = null;
  private nextToken = 0;
  private readonly pending = new Map<ObjectId, PendingStart>();
  private readonly veiled = new Set<ObjectId>();

  constructor(
    private readonly publishPending: (starts: PendingStart[]) => void,
    private tier: CardVfxTier,
  ) {}

  // Every mount starts from scratch, so StrictMode's mount → unmount → mount
  // leaves a working layer. The settled back image outlives a remount: its
  // loader stays rendered and will not load again.
  mount(canvas: HTMLCanvasElement) {
    const lifecycle = { disposed: false };
    this.lifecycle = lifecycle;
    this.canvas = canvas;
    this.state = "idle";
    this.initScheduled = false;
    // Prefetch: fetch, parse and evaluate the scene module now, so the first
    // init pays none of it. Loading it creates no renderer.
    import("./cardVfxScene.ts").then(
      (module) => {
        if (lifecycle.disposed) return;
        this.sceneModule = module;
        this.scheduleInit();
      },
      () => {
        if (!lifecycle.disposed) this.state = "failed";
      },
    );
  }

  unmount() {
    this.lifecycle.disposed = true;
    cancelAnimationFrame(this.initFrame);
    clearTimeout(this.initTask);
    this.releaseAll();
    this.scene?.dispose();
    this.scene = null;
    this.canvas = null;
    this.state = "idle";
  }

  setTier(tier: CardVfxTier) {
    this.tier = tier;
    this.scene?.setPixelRatio(pixelRatioFor(tier));
  }

  readonly backSettled = (image: HTMLImageElement | null) => {
    if (this.backImage !== undefined) return;
    this.backImage = image;
    this.scheduleInit();
  };

  present(spec: CardFlightSpec, classic: () => void) {
    switch (this.state) {
      case "idle":
        // The first effect of a mount presents Classic and starts init.
        classic();
        this.state = "initializing";
        this.scheduleInit();
        return;
      case "initializing":
      case "lost":
      case "failed":
        classic();
        return;
      case "ready":
        if (this.scene) this.presentReady(this.scene, spec, classic);
        else classic();
        return;
    }
  }

  readonly faceReady = (objectId: ObjectId, token: number, image: HTMLImageElement) => {
    if (this.pending.get(objectId)?.token !== token) return;
    image.decode().then(
      () => this.finishPending(objectId, token, image),
      () => this.fallBack(objectId, token),
    );
  };

  readonly fallBack = (objectId: ObjectId, token: number) => {
    const pending = this.takePending(objectId, token);
    if (pending) runAll(pending.classics);
  };

  private scheduleInit() {
    const { canvas, sceneModule, backImage } = this;
    if (this.state !== "initializing" || this.initScheduled) return;
    if (!canvas || !sceneModule || backImage === undefined) return;
    this.initScheduled = true;
    const callbacks = this.sceneCallbacks(backImage);
    // After the next paint: a task posted straight from `present` would run
    // before the Classic presentation's first frame paints.
    this.initFrame = requestAnimationFrame(() => {
      this.initTask = setTimeout(sceneModule.initCardVfxScene, 0, canvas, callbacks);
    });
  }

  private sceneCallbacks(backImage: HTMLImageElement | null): CardVfxSceneCallbacks {
    const { lifecycle } = this;
    return {
      backImage,
      onReady: (scene) => {
        if (lifecycle.disposed) {
          scene.dispose();
          return;
        }
        this.scene = scene;
        scene.setPixelRatio(pixelRatioFor(this.tier));
        this.state = "ready";
      },
      onFailed: () => {
        if (!lifecycle.disposed) this.state = "failed";
      },
      onContextLost: () => {
        if (lifecycle.disposed) return;
        this.state = "lost";
        this.releaseAll();
      },
      onContextRestored: () => {
        if (!lifecycle.disposed) this.state = "initializing";
      },
    };
  }

  private presentReady(scene: CardVfxScene, spec: CardFlightSpec, classic: () => void) {
    const { objectId } = spec;
    const pending = this.pending.get(objectId);
    if (pending) {
      this.join(scene, pending, spec, classic);
      return;
    }
    const face = spec.endFace ?? spec.startFace;
    if ((flipFor(spec) !== "none" || !face) && !scene.hasBack()) {
      classic();
      return;
    }
    const from = this.measureSource(spec);
    if (!from && !scene.hasFlight(objectId)) {
      classic();
      return;
    }
    if (!face) {
      // Back only: nothing to load, so the flight starts now.
      this.start(scene, spec, from, null, [classic]);
      return;
    }
    const token = ++this.nextToken;
    const deadlineMs = Math.min(CARD_FLIGHT_FACE_READY_MAX_MS, FACE_READY_STEP_FRACTION * spec.owningStepMs);
    this.pending.set(objectId, {
      token,
      spec,
      from,
      face,
      classics: [classic],
      deadline: setTimeout(() => this.fallBack(objectId, token), deadlineMs),
    });
    this.publish();
  }

  // A cast and its resolution can share one step, so the resolution may
  // arrive while the cast still waits for its face: one flight carries both.
  private join(scene: CardVfxScene, pending: PendingStart, spec: CardFlightSpec, classic: () => void) {
    pending.classics.push(classic);
    const merged = { ...spec, startFace: pending.spec.startFace };
    const face = merged.endFace ?? merged.startFace;
    const sameFace = face !== null && faceKey(face) === faceKey(pending.face);
    if (!sameFace || (flipFor(merged) !== "none" && !scene.hasBack())) {
      this.fallBack(spec.objectId, pending.token);
      return;
    }
    pending.spec = merged;
  }

  private finishPending(objectId: ObjectId, token: number, image: HTMLImageElement) {
    const pending = this.takePending(objectId, token);
    if (!pending) return;
    if (this.state !== "ready" || !this.scene) {
      runAll(pending.classics);
      return;
    }
    this.start(this.scene, pending.spec, pending.from, this.scene.uploadFace(image), pending.classics);
  }

  private takePending(objectId: ObjectId, token: number): PendingStart | null {
    const pending = this.pending.get(objectId);
    if (pending?.token !== token) return null;
    clearTimeout(pending.deadline);
    this.pending.delete(objectId);
    this.publish();
    return pending;
  }

  private start(
    scene: CardVfxScene,
    spec: CardFlightSpec,
    from: CardPose | null,
    front: Texture | null,
    classics: readonly (() => void)[],
  ) {
    const { objectId, route } = spec;
    const started = scene.startCardFlight({
      objectId,
      route,
      from,
      front,
      flip: flipFor(spec),
      pace: spec.pace,
      tier: this.tier,
      aim: (origin) => this.aim(route, objectId, origin),
      commitEpoch: () => useGameStore.getState().engineCommitEpoch,
      onRelease: () => this.unveil(objectId),
    });
    if (!started) {
      front?.dispose();
      runAll(classics);
      return;
    }
    // A handoff keeps the veil it already holds: no unveil in between.
    if (this.veiled.has(objectId)) return;
    this.veiled.add(objectId);
    useAnimationStore.getState().veilFlight(objectId);
  }

  private measureSource(spec: CardFlightSpec): CardPose | null {
    const el = sourceElement(spec.route, spec.objectId);
    if (!el || !this.canvas) return null;
    const pose = measureCardPose(el, this.canvas.getBoundingClientRect());
    if (spec.route.kind !== "cast") this.lastStackPose = pose;
    return pose;
  }

  private aim(route: CardFlightRoute, objectId: ObjectId, origin: DOMRectReadOnly): Aim {
    const aim = resolveAim(route, objectId, origin, this.lastStackPose);
    if (route.kind === "cast" && aim.kind !== "hold" && aim.el) this.lastStackPose = aim.pose;
    return aim;
  }

  private unveil(objectId: ObjectId) {
    if (!this.veiled.delete(objectId)) return;
    useAnimationStore.getState().unveilFlight(objectId);
  }

  // Context loss and unmount: every waiting event presents Classic, and every
  // veil this layer holds is released.
  private releaseAll() {
    const waiting = [...this.pending.values()];
    for (const pending of waiting) clearTimeout(pending.deadline);
    this.pending.clear();
    this.publish();
    for (const pending of waiting) runAll(pending.classics);
    const { unveilFlight } = useAnimationStore.getState();
    for (const objectId of this.veiled) unveilFlight(objectId);
    this.veiled.clear();
  }

  private publish() {
    this.publishPending([...this.pending.values()]);
  }
}

const CANVAS_STYLE: CSSProperties = {
  position: "fixed",
  inset: 0,
  width: "100%",
  height: "100%",
  pointerEvents: "none",
  // Above the board grid's stacking context (hand and stack), below modals.
  zIndex: 45,
  visibility: "hidden",
};

/** Loads the card back the DOM shows and reports it once: the loaded image,
 *  or `null` when every source failed. */
function CardBackLoader({ onSettled }: { onSettled: (image: HTMLImageElement | null) => void }) {
  const { src, isLoading, advanceFailedSource } = useCardBackImage();

  useEffect(() => {
    if (!isLoading && !src) onSettled(null);
  }, [isLoading, onSettled, src]);

  if (!src) return null;
  return (
    <img
      src={src}
      alt=""
      crossOrigin="anonymous"
      onLoad={(event) => onSettled(event.currentTarget)}
      onError={() => advanceFailedSource?.(src)}
    />
  );
}

export const CardVfxLayer = forwardRef<CardVfxLayerHandle, CardVfxLayerProps>(
  function CardVfxLayer({ tier }, ref) {
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const [pendingStarts, setPendingStarts] = useState<PendingStart[]>([]);
    const [controller] = useState(() => new CardVfxController(setPendingStarts, tier));

    useImperativeHandle(ref, () => ({
      present: (spec, classic) => controller.present(spec, classic),
    }), [controller]);

    useEffect(() => {
      const canvas = canvasRef.current;
      if (!canvas) return;
      controller.mount(canvas);
      return () => controller.unmount();
    }, [controller]);

    useEffect(() => {
      controller.setTier(tier);
    }, [controller, tier]);

    return (
      <>
        <canvas ref={canvasRef} data-card-vfx aria-hidden="true" style={CANVAS_STYLE} />
        <div hidden aria-hidden="true">
          <CardBackLoader onSettled={controller.backSettled} />
          {pendingStarts.map(({ token, spec, face }) => (
            <ResolvedAnimationImage
              key={token}
              snapshot={face}
              size="normal"
              alt=""
              fallback={null}
              crossOrigin="anonymous"
              onReady={(image) => controller.faceReady(spec.objectId, token, image)}
              onExhausted={() => controller.fallBack(spec.objectId, token)}
            />
          ))}
        </div>
      </>
    );
  },
);
