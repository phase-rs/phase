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
  permanentSurface,
  playerHudSurface,
  resolveAim,
  sourceElement,
  zoneSurface,
} from "./cardAnchors.ts";
import type { BoardSurface, CardVfxTier, FlightFlip } from "./cardFlight.ts";
import type { CardFlightRoute, CardFlightSpec, SweepSpec } from "./cardFlightSpecs.ts";
import type { CardVfxScene, CardVfxSceneCallbacks } from "./cardVfxScene.ts";
import type * as CardVfxSceneModule from "./cardVfxScene.ts";
import type {
  BoardEffectSpec,
  CardVfxSpec,
  CounterChangeSpec,
  CounterRippleSpec,
  DamageBlowSpec,
  DamageKnockbackSpec,
  DamageStrikeSpec,
  ExileDissolveSpec,
  LifeChangeSpec,
} from "./cardVfxSpecs.ts";
import type { LinkAim } from "./exileDissolve.ts";
import { drawSurface, measureSurfaceLayout } from "./surfaceTexture.ts";
import { sweepPath } from "./sweepPath.ts";

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

/** The scene runs at most one flight and one board effect per object; each
 *  may hold its veil. */
type VeilSlot = "flight" | "board";
type VeilHolds = Record<VeilSlot, number | null>;

/** Whether the snapshot a step animates, or a newer one, has committed. */
function committed(snapshotSeq: number) {
  return useGameStore.getState().lastCommittedSeq >= snapshotSeq;
}

/** A flight waiting for its face image. Later presents for the same object
 *  join it: one flight from the first source to the last destination. */
interface PendingStart {
  token: number;
  /** When the flight was presented, on the frame clock: its delay and wash
   *  count from here, however long its face takes to load. */
  presentedMs: number;
  spec: CardFlightSpec;
  from: CardPose | null;
  face: AnimationImageSnapshot;
  classics: (() => void)[];
  /** The board surface the card rests as while it waits to leave the
   *  battlefield: `undefined` while it loads, `null` to rest as the card. */
  rest: BoardSurface | null | undefined;
  /** The loaded face, while the rest still loads. */
  image: HTMLImageElement | null;
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
  /** Board effects waiting for their face, each with the covered events' Classics. */
  private readonly pendingBoard = new Map<ObjectId, (() => void)[]>();
  /** Each veiled object's holds: its flight's, its board effect's, or both. */
  private readonly veilHolds = new Map<ObjectId, VeilHolds>();
  /** Each object's running board effect. The scene runs one per object and
   *  silently disposes the one it replaces, so only the latest may hold. */
  private readonly boardEffects = new Map<ObjectId, number>();
  /** Holds released only once their snapshot commits, by holder. */
  private readonly commitWaits = new Map<number, () => void>();
  /** When each running sweep reaches its permanents, on the frame clock, by
   *  the step it sweeps: its snapshot, look and members. Destructions a
   *  snapshot reports apart (split by some other event) sweep apart. */
  private readonly sweeps = new Map<string, ReadonlyMap<ObjectId, number>>();
  /** When each countering spell whose ripple is running may leave the stack,
   *  on the frame clock. */
  private readonly counterLeaves = new Map<ObjectId, number>();
  /** Successful fire copies in the current engine batch, carrying the scorch
   *  point into a later source-free destruction. */
  private fireBatch: { snapshotSeq: number; hits: Map<ObjectId, { impact: { u: number; v: number }; amount: number }> } | null = null;

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
          case "knockback":
            this.presentKnockback(spec, classic);
            return;
          case "ripple":
            this.presentRipple(this.scene, spec, classic);
            return;
          case "life":
          case "counter":
            this.presentTally(this.scene, spec, classic);
            return;
          case "shatter":
          case "dissolve":
            this.presentBoardEffect(this.scene, spec, classic);
            return;
          case "covered":
            this.presentCovered(spec.objectId, classic);
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
    if (!pending) return;
    pending.rest?.surface.dispose();
    runAll(pending.classics);
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
    const presentedMs = performance.now();
    const sweptAtMs = this.sweepArrivalMs(scene, spec);
    if (sweptAtMs !== null) spec = { ...spec, delayMs: spec.delayMs + Math.max(0, sweptAtMs - presentedMs) };
    const leaveAtMs = spec.route.from === "Stack" ? this.counterLeaves.get(objectId) : undefined;
    if (leaveAtMs !== undefined) {
      this.counterLeaves.delete(objectId);
      spec = { ...spec, delayMs: Math.max(spec.delayMs, leaveAtMs - presentedMs) };
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
      this.start(scene, spec, presentedMs, from, null, null, [classic]);
      return;
    }
    const token = this.requestFace(
      face,
      "normal",
      spec.owningStepMs,
      (image) => this.finishPending(objectId, token, image),
      () => this.fallBack(objectId, token),
    );
    // A card waiting to leave the battlefield rests as its tile until it goes.
    const tile = spec.route.from === "Battlefield" && spec.delayMs > 0 ? ownPermanentSurface(objectId) : null;
    const waiting: PendingStart = {
      token,
      presentedMs,
      spec,
      from,
      face,
      classics: [classic],
      rest: tile ? undefined : null,
      image: null,
    };
    this.pending.set(objectId, waiting);
    if (!tile) return;
    const settle = (rest: BoardSurface | null) => {
      if (this.pending.get(objectId) !== waiting) {
        rest?.surface.dispose();
        return;
      }
      waiting.rest = rest;
      if (waiting.image) this.finishPending(objectId, token, waiting.image);
    };
    this.withBoardSurface(tile, spec.startFace, spec.owningStepMs, (_, board) => settle(board), () => settle(null));
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

  private presentBoardEffect(scene: CardVfxScene, spec: BoardEffectSpec, classic: () => void) {
    const { objectId } = spec;
    const el = ownPermanentSurface(objectId);
    if (!el) {
      classic();
      return;
    }
    const sweptAtMs = spec.kind === "shatter" ? this.sweepArrivalMs(scene, spec) : null;
    // Covered events wait on this effect while its face loads.
    const covered: (() => void)[] = [];
    this.pendingBoard.set(objectId, covered);
    const settle = () => {
      if (this.pendingBoard.get(objectId) === covered) this.pendingBoard.delete(objectId);
    };
    this.withBoardSurface(
      el,
      spec.face,
      spec.owningStepMs,
      (scene, surface) => {
        settle();
        const board = { ...surface, objectId, tier: this.tier, pace: spec.pace };
        const holder = this.startBoardEffect(objectId);
        const release = () => this.releaseAfterCommit(objectId, "board", holder, spec.snapshotSeq);
        switch (spec.kind) {
          case "shatter": {
            // A face that loads after the front has passed breaks from its first crack.
            const startMs = sweptAtMs === null ? null : Math.max(sweptAtMs, performance.now());
            const fire = spec.destroyerId === null && this.fireBatch?.snapshotSeq === spec.snapshotSeq
              ? this.fireBatch.hits.get(objectId) : undefined;
            this.fireBatch?.hits.delete(objectId);
            if (fire) scene.startBurn({ ...board, ...fire, startMs, onDone: release });
            else scene.startShatter({ ...board, impact: shatterImpact(), startMs, onDone: release });
            break;
          }
          case "dissolve":
            scene.startDissolve({ ...board, look: spec.look, link: this.linkAimFor(spec), onArrive: release });
            break;
        }
        this.hold(objectId, "board", holder);
      },
      () => {
        settle();
        classic();
        runAll(covered);
      },
    );
  }

  // A covered event's move is still loading: its Classic runs if the move
  // falls back. A move that is running holds the object's veil. With neither,
  // the move presented Classic, and so does the covered event.
  private presentCovered(objectId: ObjectId, classic: () => void) {
    const waiting = this.pending.get(objectId)?.classics ?? this.pendingBoard.get(objectId);
    if (waiting) waiting.push(classic);
    else if (!this.veilHolds.has(objectId)) classic();
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
        // A board effect this hit replaces never completes. While its hold
        // stands, until the hit's impact takes it over, the hit shows the
        // permanent in its place.
        const atRest = this.veilHolds.get(objectId)?.board != null;
        const holder = this.startBoardEffect(objectId);
        const impact = liveScene.startDamageStrike({
          ...strike,
          hit: { ...surface, objectId, atRest, onDone: () => this.release(objectId, "board", holder) },
          onImpact: () => {
            // A hit already replaced has no copy left to hold for.
            if (this.boardEffects.get(objectId) === holder) this.hold(objectId, "board", holder);
            if (spec.cause === "fire") {
              if (this.fireBatch?.snapshotSeq !== spec.snapshotSeq) {
                this.fireBatch = { snapshotSeq: spec.snapshotSeq, hits: new Map() };
              }
              this.fireBatch.hits.set(objectId, { impact, amount: spec.amount });
            }
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
    const targetEl = "Player" in target ? playerHudSurface(target.Player) : permanentSurface(target.Object);
    if (!targetEl || !this.canvas || spec.pace <= 0) {
      classic();
      return;
    }
    const canvasRect = this.canvas.getBoundingClientRect();
    const sourceEl = sourceId === null ? null : permanentSurface(sourceId);
    scene.startDamageBlow({
      from: sourceEl && measureCardPose(sourceEl, canvasRect),
      to: measureCardPose(targetEl, canvasRect),
      amount: spec.amount,
      tier: this.tier,
      pace: spec.pace,
      startMs: spec.startMs,
      impactS: spec.impactDelayMs / 1000 / spec.pace,
    });
  }

  // The struck permanent is measured now, before the slam reaches it. Its copy
  // hides it only from the impact on; the slamming creature, which the DOM
  // still draws, lies over it, so the copy leaves out wherever it covers it.
  private presentKnockback(spec: DamageKnockbackSpec, classic: () => void) {
    const { objectId, sourceId } = spec;
    const el = ownPermanentSurface(objectId);
    if (!el || !this.canvas) {
      classic();
      return;
    }
    const attacker = permanentSurface(sourceId);
    const from = attacker && measureCardPose(attacker, this.canvas.getBoundingClientRect());
    const attackerRadius = (attacker && measureSurfaceLayout(attacker)?.radius) ?? 0;
    const occluder =
      attacker &&
      ((origin: DOMRectReadOnly) => {
        const pose = measureCardPose(attacker, origin);
        return { pose, radius: (attackerRadius * pose.w) / (attacker.offsetWidth || pose.w) };
      });
    this.withBoardSurface(
      el,
      spec.face,
      spec.owningStepMs,
      (scene, surface) => {
        const atRest = this.veilHolds.get(objectId)?.board != null;
        const holder = this.startBoardEffect(objectId);
        scene.startDamageKnockback({
          objectId,
          atRest,
          hit: { ...surface, onDone: () => this.release(objectId, "board", holder) },
          from,
          occluder,
          amount: spec.amount,
          pace: spec.pace,
          startMs: spec.startMs,
          impactS: spec.impactDelayMs / 1000 / spec.pace,
          onImpact: () => {
            // A copy already replaced has nothing left to hold for.
            if (this.boardEffects.get(objectId) === holder) this.hold(objectId, "board", holder);
          },
        });
      },
      classic,
    );
  }

  // Both stack entries are measured now: the commit takes them off the stack.
  // The counter leaves once the spell it counters has washed out, though the
  // engine reports its move after the counter's own resolution, a step later.
  private presentRipple(scene: CardVfxScene, spec: CounterRippleSpec, classic: () => void) {
    const { origin } = spec;
    const source = zoneSurface(origin.zone, origin.objectId, origin.ownerId);
    const target = zoneSurface("Stack", spec.targetId, origin.ownerId);
    if (!source || !target || !this.canvas) {
      classic();
      return;
    }
    const canvasRect = this.canvas.getBoundingClientRect();
    scene.startCounterRipple({
      from: measureCardPose(source, canvasRect),
      to: measureCardPose(target, canvasRect),
      look: spec.look,
      tier: this.tier,
      pace: spec.pace,
    });
    if (origin.zone !== "Stack") return;
    const now = performance.now();
    for (const [objectId, leaveAtMs] of this.counterLeaves) {
      if (leaveAtMs < now) this.counterLeaves.delete(objectId);
    }
    this.counterLeaves.set(origin.objectId, now + spec.leaveMs);
  }

  // A life or counter change plays over its HUD or permanent where it is now.
  private presentTally(scene: CardVfxScene, spec: LifeChangeSpec | CounterChangeSpec, classic: () => void) {
    const el = spec.kind === "life" ? playerHudSurface(spec.playerId) : permanentSurface(spec.objectId);
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

  /** When the front of `spec`'s sweep reaches its permanent, on the frame
   *  clock; `null` when it has no sweep. The step's first swept permanent to
   *  present measures every member where it lies now and sets the front out;
   *  the rest take their times from it. Fewer than two members still on the
   *  board make no front, and each then breaks or leaves at once. */
  private sweepArrivalMs(
    scene: CardVfxScene,
    { objectId, sweep, snapshotSeq, pace }: { objectId: ObjectId; sweep: SweepSpec | null; snapshotSeq: number; pace: number },
  ): number | null {
    if (!sweep || !this.canvas) return null;
    const key = `${snapshotSeq}:${sweep.look}:${sweep.memberIds.join(",")}`;
    let arrivals = this.sweeps.get(key);
    if (!arrivals) {
      const origin = this.canvas.getBoundingClientRect();
      const members = new Map(
        sweep.memberIds.flatMap((id) => {
          const el = ownPermanentSurface(id);
          return el ? [[id, measureCardPose(el, origin)] as const] : [];
        }),
      );
      arrivals = new Map();
      if (members.size >= 2) {
        const hud = playerHudSurface(sweep.casterId);
        const path = sweepPath(hud && measureCardPose(hud, origin), members);
        const startMs = performance.now();
        scene.startBoardSweep({ path, look: sweep.look, tier: this.tier, pace, startMs });
        arrivals = new Map(path.hits.map((hit) => [hit.objectId, startMs + hit.atS * 1000 * pace]));
      }
      // Only the step being animated can still present a member.
      for (const other of this.sweeps.keys()) {
        if (!other.startsWith(`${snapshotSeq}:`)) this.sweeps.delete(other);
      }
      this.sweeps.set(key, arrivals);
    }
    return arrivals.get(objectId) ?? null;
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
    const waiting = this.pending.get(objectId);
    if (waiting?.token === token && waiting.rest === undefined) {
      waiting.image = image;
      return;
    }
    const pending = this.takePending(objectId, token);
    if (!pending) return;
    if (this.state !== "ready" || !this.scene) {
      pending.rest?.surface.dispose();
      runAll(pending.classics);
      return;
    }
    const { spec, presentedMs, from, rest, classics } = pending;
    this.start(this.scene, spec, presentedMs, from, this.scene.uploadFace(image), rest ?? null, classics);
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
    presentedMs: number,
    from: CardPose | null,
    front: Texture | null,
    rest: BoardSurface | null,
    classics: readonly (() => void)[],
  ) {
    const { objectId, route, snapshotSeq, wash } = spec;
    const holder = this.nextToken++;
    const started = scene.startCardFlight({
      objectId,
      route,
      from,
      front,
      flip: flipFor(spec),
      pace: spec.pace,
      delayMs: Math.max(0, presentedMs + spec.delayMs - performance.now()),
      wash: wash && { startMs: presentedMs + wash.atMs, durationMs: wash.durationMs, look: wash.look },
      rest,
      tier: this.tier,
      landingColors: spec.endColors,
      aim: (origin) => this.aim(route, objectId, origin),
      committed: () => committed(snapshotSeq),
      // A pile stands in for its card before the commit moves it there, so
      // the card's old surface stays veiled until then.
      onRelease: () => this.releaseAfterCommit(objectId, "flight", holder, snapshotSeq),
    });
    if (!started) {
      front?.dispose();
      rest?.surface.dispose();
      runAll(classics);
      return;
    }
    this.hold(objectId, "flight", holder);
  }

  private startBoardEffect(objectId: ObjectId) {
    const holder = this.nextToken++;
    this.boardEffects.set(objectId, holder);
    return holder;
  }

  // An object stays veiled while its flight or its board effect holds it. A
  // new effect in a slot takes over its predecessor's hold, and any commit
  // wait it was held under: a handoff, with no unveil in between.
  private hold(objectId: ObjectId, slot: VeilSlot, holder: number) {
    const holds = this.veilHolds.get(objectId);
    if (!holds) {
      this.veilHolds.set(objectId, { flight: null, board: null, [slot]: holder });
      useAnimationStore.getState().veilFlight(objectId);
      return;
    }
    const previous = holds[slot];
    if (previous !== null) this.cancelCommitWait(previous);
    holds[slot] = holder;
  }

  /** Releases `holder`'s hold once the snapshot its step animates has committed,
   *  so an effect that ends first does not show the card's old surface again. */
  private releaseAfterCommit(objectId: ObjectId, slot: VeilSlot, holder: number, snapshotSeq: number) {
    if (this.veilHolds.get(objectId)?.[slot] !== holder) return;
    if (committed(snapshotSeq)) {
      this.release(objectId, slot, holder);
      return;
    }
    const stop = useGameStore.subscribe((state) => {
      if (state.lastCommittedSeq < snapshotSeq) return;
      this.cancelCommitWait(holder);
      this.release(objectId, slot, holder);
    });
    this.commitWaits.set(holder, stop);
  }

  private cancelCommitWait(holder: number) {
    this.commitWaits.get(holder)?.();
    this.commitWaits.delete(holder);
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

  /** Releases `holder`'s hold, if it still has it, and unveils the object
   *  once nothing holds it. */
  private release(objectId: ObjectId, slot: VeilSlot, holder: number) {
    if (slot === "board" && this.boardEffects.get(objectId) === holder) this.boardEffects.delete(objectId);
    const holds = this.veilHolds.get(objectId);
    if (holds?.[slot] !== holder) return;
    holds[slot] = null;
    if (holds.flight !== null || holds.board !== null) return;
    this.veilHolds.delete(objectId);
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
    for (const stop of this.commitWaits.values()) stop();
    this.commitWaits.clear();
    const { unveilFlight } = useAnimationStore.getState();
    for (const objectId of this.veilHolds.keys()) unveilFlight(objectId);
    this.veilHolds.clear();
    this.boardEffects.clear();
    this.sweeps.clear();
    this.counterLeaves.clear();
    this.fireBatch = null;
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
