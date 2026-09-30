// ─── Card VFX layer ───
// The React side of the shared WebGL overlay: the canvas, the hidden image
// loaders, and the `present` handle through which AnimationOverlay offers each
// event with a card VFX spec. Every `present` ends in exactly one presentation —
// a GL effect, or the Classic effect it was handed — and the animation queue
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
import type { CardImageSource } from "../../../services/visualPacks/types.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { type AnimationImageSnapshot, ResolvedAnimationImage } from "../ResolvedAnimationImage.tsx";
import {
  type Aim,
  type CardPose,
  exileGhostNode,
  measureCardPose,
  ownPermanentSurface,
  playerHudSurface,
  resolveAim,
  sourceElement,
  zoneSurface,
} from "./cardAnchors.ts";
import type { CardVfxTier, FlightFlip } from "./cardFlight.ts";
import type { CardFlightRoute, CardFlightSpec } from "./cardFlightSpecs.ts";
import type { CardVfxScene, CardVfxSceneCallbacks } from "./cardVfxScene.ts";
import type * as CardVfxSceneModule from "./cardVfxScene.ts";
import type {
  BoardEffectSpec,
  CardVfxSpec,
  CounterChangeSpec,
  DamageBlowSpec,
  DamageStrikeSpec,
  ExileDissolveSpec,
  LifeChangeSpec,
} from "./cardVfxSpecs.ts";
import type { LinkAim } from "./exileDissolve.ts";
import { drawSurface, measureSurfaceLayout } from "./surfaceTexture.ts";

/** An effect whose face has not loaded by this deadline presents Classic. */
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
  /** Presents `spec` as a GL effect, or runs `classic` instead. Exactly one
   *  of the two happens, once. A GL damage strike runs `onImpact` when its hit
   *  lands; the Classic presentation shows its own impact. */
  present(spec: CardVfxSpec, classic: () => void, onImpact?: () => void): void;
}

interface CardVfxLayerProps {
  tier: CardVfxTier;
}

type LayerState = "idle" | "initializing" | "ready" | "lost" | "failed";

/** A face image the layer is loading. Exactly one of `loaded` and `failed`
 *  runs: `failed` when the image errors, misses its deadline, or the layer
 *  releases its work. */
interface FaceRequest {
  token: number;
  face: AnimationImageSnapshot;
  size: "normal" | "art_crop";
  deadline: ReturnType<typeof setTimeout>;
  loaded(image: HTMLImageElement): void;
  failed(): void;
}

/** A flight waiting for its face image. Later presents for the same object
 *  join it: one flight from the first source to the last destination. */
interface PendingStart {
  token: number;
  spec: CardFlightSpec;
  from: CardPose | null;
  face: AnimationImageSnapshot;
  classics: (() => void)[];
}

/** A permanent's surface redrawn as a texture, and where it lies. */
interface BoardSurface {
  pose: CardPose;
  surface: Texture;
  radius: number;
}

/** Where a shatter breaks, as fractions of the card: somewhere central. */
function shatterImpact() {
  return { u: 0.3 + Math.random() * 0.4, v: 0.25 + Math.random() * 0.45 };
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
  private readonly faceRequests = new Map<number, FaceRequest>();
  private readonly pending = new Map<ObjectId, PendingStart>();
  private readonly veiled = new Set<ObjectId>();
  private readonly commitWaits = new Set<() => void>();

  constructor(
    private readonly publishRequests: (requests: FaceRequest[]) => void,
    private tier: CardVfxTier,
  ) {}

  // Every mount starts from scratch, so StrictMode's mount → unmount → mount
  // leaves a working layer. The settled back image outlives a remount: its
  // loader stays rendered and will not load again.
  mount(canvas: HTMLCanvasElement) {
    const lifecycle = { disposed: false };
    this.lifecycle = lifecycle;
    this.canvas = canvas;
    this.setState("idle");
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
        if (!lifecycle.disposed) this.setState("failed");
      },
    );
  }

  unmount() {
    this.lifecycle.disposed = true;
    cancelAnimationFrame(this.initFrame);
    clearTimeout(this.initTask);
    // The scene first: disposing a strike that has not landed lands it, and
    // releasing then drops the veil that landing takes.
    this.scene?.dispose();
    this.scene = null;
    this.releaseAll();
    this.canvas = null;
    this.setState("idle");
  }

  setTier(tier: CardVfxTier) {
    this.tier = tier;
    this.scene?.setPixelRatio(pixelRatioFor(tier));
  }

  // Publishes readiness, so hit timing and spell announcements follow the
  // presentation this layer will actually give.
  private setState(state: LayerState) {
    this.state = state;
    useAnimationStore.getState().setCardVfxReady(state === "ready");
  }

  readonly backSettled = (image: HTMLImageElement | null) => {
    if (this.backImage !== undefined) return;
    this.backImage = image;
    this.scheduleInit();
  };

  present(spec: CardVfxSpec, classic: () => void, onImpact: () => void = () => {}) {
    switch (this.state) {
      case "idle":
        // The first effect of a mount presents Classic and starts init.
        classic();
        this.setState("initializing");
        this.scheduleInit();
        return;
      case "initializing":
      case "lost":
      case "failed":
        classic();
        return;
      case "ready":
        if (!this.scene) {
          classic();
          return;
        }
        switch (spec.kind) {
          case "flight":
            this.presentFlight(this.scene, spec, classic);
            return;
          case "damage":
            this.presentDamage(this.scene, spec, classic, onImpact);
            return;
          case "blow":
            this.presentBlow(this.scene, spec, classic);
            return;
          case "life":
          case "counter":
            this.presentTally(this.scene, spec, classic);
            return;
          case "shatter":
          case "dissolve":
            this.presentBoardEffect(spec, classic);
            return;
          case "covered":
            return;
        }
    }
  }

  readonly faceReady = (token: number, image: HTMLImageElement) => {
    if (!this.faceRequests.has(token)) return;
    image.decode().then(
      () => this.takeFace(token)?.loaded(image),
      () => this.faceFailed(token),
    );
  };

  readonly faceFailed = (token: number) => {
    this.takeFace(token)?.failed();
  };

  private requestFace(
    face: AnimationImageSnapshot,
    size: FaceRequest["size"],
    owningStepMs: number,
    loaded: (image: HTMLImageElement) => void,
    failed: () => void,
  ): number {
    const token = ++this.nextToken;
    const deadlineMs = Math.min(CARD_FLIGHT_FACE_READY_MAX_MS, FACE_READY_STEP_FRACTION * owningStepMs);
    const deadline = setTimeout(() => this.faceFailed(token), deadlineMs);
    this.faceRequests.set(token, { token, face, size, deadline, loaded, failed });
    this.publish();
    return token;
  }

  private takeFace(token: number): FaceRequest | null {
    const request = this.faceRequests.get(token);
    if (!request) return null;
    clearTimeout(request.deadline);
    this.faceRequests.delete(token);
    this.publish();
    return request;
  }

  private fallBack(objectId: ObjectId, token: number) {
    const pending = this.takePending(objectId, token);
    if (pending) runAll(pending.classics);
  }

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
        this.setState("ready");
      },
      onFailed: () => {
        if (!lifecycle.disposed) this.setState("failed");
      },
      onContextLost: () => {
        if (lifecycle.disposed) return;
        this.setState("lost");
        this.releaseAll();
      },
      onContextRestored: () => {
        if (!lifecycle.disposed) this.setState("initializing");
      },
    };
  }

  private presentFlight(scene: CardVfxScene, spec: CardFlightSpec, presentClassic: () => void) {
    const { objectId } = spec;
    // A Classic presentation supersedes X's earlier landing, so it ends that
    // flight's reveal first. Dropping at the top of `presentReady` instead would
    // expose the unloaded own node while a GL flight waits for its face; on the
    // GL path `startCardFlight` disposes the released flight as the new one starts.
    const classic = () => {
      scene.dropReleasedFlight(objectId);
      presentClassic();
    };
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
    const token = this.requestFace(
      face,
      "normal",
      spec.owningStepMs,
      (image) => this.finishPending(objectId, token, image),
      () => this.fallBack(objectId, token),
    );
    this.pending.set(objectId, { token, spec, from, face, classics: [classic] });
  }

  // The permanent's surface is measured now, while it is still on the board;
  // its face loads in the size the board shows, and the surface is redrawn
  // from both so the first GL frame matches the card it replaces. Exactly one
  // of `ready` and `classic` runs.
  private withBoardSurface(
    el: HTMLElement,
    face: AnimationImageSnapshot | null,
    owningStepMs: number,
    ready: (scene: CardVfxScene, board: BoardSurface) => void,
    classic: () => void,
  ) {
    const layout = measureSurfaceLayout(el);
    if (!layout || !face || !this.canvas) {
      classic();
      return;
    }
    const pose = measureCardPose(el, this.canvas.getBoundingClientRect());
    // The board shows art crops except for tokens, whose art-crop tiles use the full image.
    const artCrop = usePreferencesStore.getState().battlefieldCardDisplay === "art_crop" && !face.isToken;
    this.requestFace(
      face,
      artCrop ? "art_crop" : "normal",
      owningStepMs,
      (image) => {
        const { scene } = this;
        if (this.state !== "ready" || !scene) {
          classic();
          return;
        }
        const surface = scene.uploadFace(drawSurface(layout, image, pixelRatioFor(this.tier)));
        ready(scene, { pose, surface, radius: layout.radius });
      },
      classic,
    );
  }

  private presentBoardEffect(spec: BoardEffectSpec, classic: () => void) {
    const { objectId } = spec;
    const el = ownPermanentSurface(objectId);
    if (!el) {
      classic();
      return;
    }
    const commitEpoch = useGameStore.getState().engineCommitEpoch;
    this.withBoardSurface(
      el,
      spec.face,
      spec.owningStepMs,
      (scene, surface) => {
        const board = { ...surface, objectId, tier: this.tier, pace: spec.pace };
        const release = () => this.unveilAfterCommit(objectId, commitEpoch);
        switch (spec.kind) {
          case "shatter":
            scene.startShatter({ ...board, impact: shatterImpact(), onDone: release });
            break;
          case "dissolve":
            scene.startDissolve({ ...board, link: this.linkAimFor(spec), onArrive: release });
            break;
        }
        this.veil(objectId);
      },
      classic,
    );
  }

  // Source and target are measured now: the source's stack entry leaves the
  // board when the engine commit lands. A struck permanent is veiled from the
  // impact until its copy has rocked back to rest.
  private presentDamage(scene: CardVfxScene, spec: DamageStrikeSpec, classic: () => void, onImpact: () => void) {
    const { origin, target } = spec;
    const source = zoneSurface(origin.zone, origin.objectId, origin.ownerId);
    const targetEl =
      target.kind === "player"
        ? playerHudSurface(target.playerId)
        : ownPermanentSurface(target.objectId);
    if (!source || !targetEl || !this.canvas) {
      classic();
      return;
    }
    const canvasRect = this.canvas.getBoundingClientRect();
    const strike = {
      cause: spec.cause,
      from: measureCardPose(source, canvasRect),
      to: measureCardPose(targetEl, canvasRect),
      amount: spec.amount,
      tier: this.tier,
      pace: spec.pace,
    };
    if (target.kind === "player") {
      scene.startDamageStrike({ ...strike, hit: null, onImpact });
      return;
    }
    const { objectId } = target;
    this.withBoardSurface(
      targetEl,
      target.face,
      spec.owningStepMs,
      (liveScene, surface) => {
        liveScene.startDamageStrike({
          ...strike,
          hit: { ...surface, objectId, onDone: () => this.unveil(objectId) },
          onImpact: () => {
            this.veil(objectId);
            onImpact();
          },
        });
      },
      classic,
    );
  }

  // Both surfaces are measured now, before the slam moves its creature.
  private presentBlow(scene: CardVfxScene, spec: DamageBlowSpec, classic: () => void) {
    const { target, sourceId } = spec;
    const targetEl = "Player" in target ? playerHudSurface(target.Player) : ownPermanentSurface(target.Object);
    if (!targetEl || !this.canvas || spec.pace <= 0) {
      classic();
      return;
    }
    const canvasRect = this.canvas.getBoundingClientRect();
    const sourceEl = sourceId === null ? null : ownPermanentSurface(sourceId);
    scene.startDamageBlow({
      from: sourceEl && measureCardPose(sourceEl, canvasRect),
      to: measureCardPose(targetEl, canvasRect),
      amount: spec.amount,
      tier: this.tier,
      pace: spec.pace,
      impactS: spec.impactDelayMs / 1000 / spec.pace,
    });
  }

  // A life or counter change plays over its HUD or permanent where it is now.
  private presentTally(scene: CardVfxScene, spec: LifeChangeSpec | CounterChangeSpec, classic: () => void) {
    const el = spec.kind === "life" ? playerHudSurface(spec.playerId) : ownPermanentSurface(spec.objectId);
    if (!el || !this.canvas || spec.pace <= 0) {
      classic();
      return;
    }
    const at = measureCardPose(el, this.canvas.getBoundingClientRect());
    const { tier } = this;
    switch (spec.kind) {
      case "life":
        scene.startLifeChange({ at, amount: spec.amount, tier, pace: spec.pace });
        return;
      case "counter":
        scene.startCounterChange({ at, counterType: spec.counterType, change: spec.change, count: spec.count, tier, pace: spec.pace });
        return;
    }
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

  /** Where a held card's flakes go: its ghost under its holder, measured each frame. */
  private linkAimFor({ objectId, holderId }: ExileDissolveSpec) {
    if (holderId === null) return null;
    const measure = (el: HTMLElement | null, origin: DOMRectReadOnly) => el && measureCardPose(el, origin);
    return (origin: DOMRectReadOnly): LinkAim => ({
      ghost: measure(exileGhostNode(objectId), origin),
      holder: measure(ownPermanentSurface(holderId), origin),
    });
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
    this.takeFace(token);
    this.pending.delete(objectId);
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
      delayMs: spec.delayMs,
      tier: this.tier,
      landingColors: spec.endColors,
      aim: (origin) => this.aim(route, objectId, origin),
      commitEpoch: () => useGameStore.getState().engineCommitEpoch,
      onRelease: () => this.unveil(objectId),
    });
    if (!started) {
      front?.dispose();
      runAll(classics);
      return;
    }
    this.veil(objectId);
  }

  // A handoff keeps the veil it already holds: no unveil in between.
  private veil(objectId: ObjectId) {
    if (this.veiled.has(objectId)) return;
    this.veiled.add(objectId);
    useAnimationStore.getState().veilFlight(objectId);
  }

  /** Unveils once the engine commit that follows `commitEpoch` has landed, so
   *  an effect that ends first does not show the card's old surface again. */
  private unveilAfterCommit(objectId: ObjectId, commitEpoch: number) {
    if (useGameStore.getState().engineCommitEpoch !== commitEpoch) {
      this.unveil(objectId);
      return;
    }
    const stop = useGameStore.subscribe((state) => {
      if (state.engineCommitEpoch === commitEpoch) return;
      stop();
      this.commitWaits.delete(stop);
      this.unveil(objectId);
    });
    this.commitWaits.add(stop);
  }

  private measureSource(spec: CardFlightSpec): CardPose | null {
    const el = sourceElement(spec.route, spec.sourceId);
    if (!el || !this.canvas) return null;
    const pose = measureCardPose(el, this.canvas.getBoundingClientRect());
    if (spec.route.from === "Stack") this.lastStackPose = pose;
    return pose;
  }

  private aim(route: CardFlightRoute, objectId: ObjectId, origin: DOMRectReadOnly): Aim {
    const aim = resolveAim(route, objectId, origin, this.lastStackPose);
    if (route.to === "Stack" && aim.kind !== "hold" && aim.el) this.lastStackPose = aim.pose;
    return aim;
  }

  private unveil(objectId: ObjectId) {
    if (!this.veiled.delete(objectId)) return;
    useAnimationStore.getState().unveilFlight(objectId);
  }

  // Context loss and unmount: every waiting event presents Classic, and every
  // veil this layer holds is released.
  private releaseAll() {
    const waiting = [...this.faceRequests.values()];
    for (const request of waiting) clearTimeout(request.deadline);
    this.faceRequests.clear();
    this.publish();
    for (const request of waiting) request.failed();
    for (const stop of this.commitWaits) stop();
    this.commitWaits.clear();
    const { unveilFlight } = useAnimationStore.getState();
    for (const objectId of this.veiled) unveilFlight(objectId);
    this.veiled.clear();
  }

  private publish() {
    this.publishRequests([...this.faceRequests.values()]);
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

export const CORS_ONLY_SEARCH_PARAM = "cors";

/** The URL the layer requests for a card back. The app's plain `<img>`s of a
 *  remote back's URL leave a response without CORS headers in the HTTP cache,
 *  which a CORS request for the same URL would reuse and fail on. A remote back
 *  is therefore requested under a query parameter no other code requests, and
 *  the CDN serves the same bytes with CORS headers. Installed pack sources are
 *  local and pass through unchanged. */
export function corsOnlySrc(source: Extract<CardImageSource, { src: string }>): string {
  if (source.kind !== "remote") return source.src;
  const url = new URL(source.src);
  url.searchParams.set(CORS_ONLY_SEARCH_PARAM, "1");
  return url.toString();
}

/** Loads the card back the DOM shows, through `corsOnlySrc`, and reports it
 *  once: the loaded image, or `null` when every source failed. */
function CardBackLoader({ onSettled }: { onSettled: (image: HTMLImageElement | null) => void }) {
  const { src, source, isLoading, advanceFailedSource } = useCardBackImage();

  useEffect(() => {
    if (!isLoading && !src) onSettled(null);
  }, [isLoading, onSettled, src]);

  if (!source || source.kind === "fallback") return null;
  return (
    <img
      src={corsOnlySrc(source)}
      alt=""
      crossOrigin="anonymous"
      onLoad={(event) => onSettled(event.currentTarget)}
      // The ladder advances on the source's own URL, not the rewritten one.
      onError={() => advanceFailedSource?.(source.src)}
    />
  );
}

export const CardVfxLayer = forwardRef<CardVfxLayerHandle, CardVfxLayerProps>(
  function CardVfxLayer({ tier }, ref) {
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const [faceRequests, setFaceRequests] = useState<FaceRequest[]>([]);
    const [controller] = useState(() => new CardVfxController(setFaceRequests, tier));

    useImperativeHandle(ref, () => ({
      present: (spec, classic, onImpact) => controller.present(spec, classic, onImpact),
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
          {faceRequests.map(({ token, face, size }) => (
            <ResolvedAnimationImage
              key={token}
              snapshot={face}
              size={size}
              alt=""
              fallback={null}
              crossOrigin="anonymous"
              onReady={(image) => controller.faceReady(token, image)}
              onExhausted={() => controller.faceFailed(token)}
            />
          ))}
        </div>
      </>
    );
  },
);
