import { type Mesh, PerspectiveCamera, Scene, type ShaderMaterial, Texture, Vector3 } from "three";
import { describe, expect, it, vi } from "vitest";

import type { Aim, CardPose } from "../cardAnchors.ts";
import {
  ABANDON_FADE_MS,
  ABSENT_FRAMES_AFTER_COMMIT,
  type BoardSurface,
  CARD_ASPECT,
  CARD_FLIGHT_MAX_AWAIT_MS,
  CAST_FLIGHT_MS,
  createCardFlight,
  flightPose,
  type FlightCurve,
  type FlightWash,
  type FlightRelease,
  HOLD_RISE_FRACTION,
  LAND_REVEAL_WAIT_MAX_MS,
  LAND_STATIONARY_WAIT_MAX_MS,
  LANDING_CROSSFADE_MS,
  RESOLVE_FLIGHT_MS,
  restingState,
  SETTLE_MS,
} from "../cardFlight.ts";
import type { CardFlightRoute } from "../cardFlightSpecs.ts";
import { type EffectHost, fitPixelCamera } from "../cardVfxScene.ts";

const X = 7;
const FRAME_MS = 16;
const CARD_H = 88;
const CARD_W = CARD_H * CARD_ASPECT;
const FROM: CardPose = { x: 100, y: 700, w: CARD_W, h: CARD_H, angleDeg: 5 };
const TO: CardPose = { x: 300, y: 200, w: CARD_W * 1.2, h: CARD_H * 1.2, angleDeg: 90 };

function expectPose(actual: CardPose, expected: CardPose) {
  expect(actual.x).toBeCloseTo(expected.x, 6);
  expect(actual.y).toBeCloseTo(expected.y, 6);
  expect(actual.w).toBeCloseTo(expected.w, 6);
  expect(actual.h).toBeCloseTo(expected.h, 6);
  expect(actual.angleDeg).toBeCloseTo(expected.angleDeg, 6);
}

function host(): EffectHost {
  return {
    scene: new Scene(),
    backTexture: new Texture(),
    placeholderTexture: new Texture(),
    canvasOrigin: () => new DOMRect(0, 0, 1000, 800),
  };
}

function own(pose: CardPose): Aim {
  return { kind: "own", el: document.createElement("div"), pose, opacity: 1, faceImagesSettled: true };
}

interface Harness {
  releases: FlightRelease[];
  flight: ReturnType<typeof createCardFlight>;
  host: EffectHost;
  /** Runs one frame at `ms` and returns whether the flight still runs. */
  frame(ms: number): boolean;
  alpha(): number | null;
}

function fly(
  route: CardFlightRoute,
  {
    aim,
    committed = () => false,
    pace = 1,
    delayMs = 0,
    wash = null,
    rest = null,
  }: {
    aim: () => Aim;
    committed?: () => boolean;
    pace?: number;
    delayMs?: number;
    wash?: FlightWash | null;
    rest?: BoardSurface | null;
  },
): Harness {
  const releases: FlightRelease[] = [];
  const effectHost = host();
  const flight = createCardFlight(effectHost, {
    objectId: X,
    route,
    from: restingState(FROM, "none"),
    front: null,
    back: effectHost.backTexture,
    flip: "none",
    pace,
    delayMs,
    wash,
    rest,
    tier: "full",
    aim,
    committed,
    onRelease: (reason) => releases.push(reason),
  });
  return {
    releases,
    flight,
    host: effectHost,
    frame: (ms) => flight.update(ms),
    alpha: () => {
      const card = effectHost.scene.getObjectByName("card-flight") as Mesh<never, ShaderMaterial> | undefined;
      return card ? (card.material.uniforms.uAlpha.value as number) : null;
    },
  };
}

function card(effectHost: EffectHost) {
  return effectHost.scene.getObjectByName("card-flight") as Mesh<never, ShaderMaterial>;
}

describe("a waiting flight", () => {
  const GRAVEYARD: CardFlightRoute = { from: "Stack", to: "Graveyard", ownerId: 0 };

  it("V14-1: a staggered card rests, drawn, on its source until its turn, then leaves", () => {
    const target = own(TO);
    const { frame, host: effectHost } = fly(GRAVEYARD, { aim: () => target, delayMs: 200 });
    frame(1000);
    frame(1150);
    const mesh = card(effectHost);
    expect(mesh.visible).toBe(true);
    expect(mesh.position.x).toBeCloseTo(FROM.x, 6);
    expect(mesh.position.y).toBeCloseTo(-FROM.y, 6);
    frame(1200);
    frame(1200 + RESOLVE_FLIGHT_MS / 2);
    expect(Math.hypot(mesh.position.x - FROM.x, mesh.position.y + FROM.y)).toBeGreaterThan(20);
  });

  it("V16-1: a card leaving a tile rests as a copy of the tile, and turns into the card as it leaves", () => {
    // A landscape art-crop tile, nothing like the card's shape.
    const tile = { pose: { x: 300, y: 400, w: 120, h: 80, angleDeg: 0 }, surface: new Texture(), radius: 6 };
    const dispose = vi.spyOn(tile.surface, "dispose");
    const { frame, host: effectHost } = fly(GRAVEYARD, { aim: () => own(TO), delayMs: 200, rest: tile });
    const rest = () => effectHost.scene.getObjectByName("card-flight-rest") as Mesh<never, ShaderMaterial> | undefined;
    frame(1000);
    frame(1150);
    expect(card(effectHost).visible).toBe(false);
    expect(rest()?.scale.toArray()).toEqual([120, 80, 1]);
    expect(rest()?.position.toArray()).toEqual([300, -400, 0]);
    expect(rest()?.material.uniforms.uRadius.value).toBe(6);
    frame(1200);
    expect(rest()).toBeUndefined();
    expect(dispose).toHaveBeenCalled();
    expect(card(effectHost).visible).toBe(true);
  });

  it("V14-2: a wash runs pale over its span from its start on the frame clock, and stays", () => {
    const wash: FlightWash = { startMs: 1300, durationMs: 200, look: "water" };
    const { frame, host: effectHost } = fly(GRAVEYARD, { aim: () => own(TO), delayMs: 600, wash });
    const uniforms = card(effectHost).material.uniforms;
    frame(1000);
    frame(1290);
    expect(uniforms.uWash.value).toBe(0);
    frame(1400);
    expect(uniforms.uWash.value).toBeGreaterThan(0.3);
    expect(uniforms.uWash.value).toBeLessThan(0.7);
    frame(1700);
    expect(uniforms.uWash.value).toBe(1);
    expect(uniforms.uWashTint.value.toArray()).toEqual([0.72, 0.85, 0.94]);
  });

  it("V14-2: an unwashed flight never washes", () => {
    const { frame, host: effectHost } = fly(GRAVEYARD, { aim: () => own(TO) });
    frame(1000);
    frame(1500);
    expect(card(effectHost).material.uniforms.uWash.value).toBe(0);
  });
});

describe("flightPose", () => {
  it.each<FlightCurve>(["panel", "land"])("V3-6a: the %s profile starts on `from` and ends on `to`", (curve) => {
    const from = restingState(FROM, "none");
    expectPose(flightPose(curve, 0, from, TO, 0), from);
    expectPose(flightPose(curve, 1, from, TO, 0), TO);
    expect(flightPose(curve, 1, from, TO, 0).z).toBeCloseTo(0, 6);
  });

  it("V3-6b: the land profile is over the slot by 72% and still in the air; the panel profile is not", () => {
    const from = restingState(FROM, "none");
    for (const t of [0.72, 0.8, 0.95]) {
      const pose = flightPose("land", t, from, TO, 0);
      expect(pose.x).toBeCloseTo(TO.x, 6);
      expect(pose.y).toBeCloseTo(TO.y, 6);
    }
    expect(flightPose("land", 0.72, from, TO, 0).z).toBeGreaterThan(0);
    const panel = flightPose("panel", 0.95, from, TO, 0);
    expect(Math.hypot(panel.x - TO.x, panel.y - TO.y)).toBeGreaterThan(0.01);
  });

  it("V3-6c: a flight from a pose to itself produces no NaN", () => {
    const from = restingState(FROM, "none");
    for (const curve of ["panel", "land"] as const) {
      for (const t of [0, 0.3, 0.72, 1]) {
        const pose = flightPose(curve, t, from, FROM, 0);
        const values = [pose.x, pose.y, pose.z, pose.w, pose.h, pose.angleDeg, ...pose.quaternion.toArray()];
        expect(values.every(Number.isFinite)).toBe(true);
      }
    }
  });

  it("V3-6c: the pixel-fit camera maps a z = 0 point to its CSS pixel", () => {
    const camera = new PerspectiveCamera();
    fitPixelCamera(camera, 390, 844);
    camera.updateMatrixWorld();
    for (const [x, y] of [[0, 0], [195, 422], [390, 844], [37.5, 700.25]]) {
      const ndc = new Vector3(x, -y, 0).project(camera);
      expect(((ndc.x + 1) / 2) * 390).toBeCloseTo(x, 6);
      expect(((1 - ndc.y) / 2) * 844).toBeCloseTo(y, 6);
    }
  });
});

describe("card flight effect", () => {
  it("V3-6d (i): a new target element rebases from the current pose and lands only on the own node", () => {
    let aim: Aim = { kind: "hold" };
    const harness = fly({ from: "Stack", to: "Battlefield", ownerId: 0 }, { aim: () => aim });
    const positions: Array<{ x: number; y: number; z: number }> = [];
    const record = () => {
      const { x, y, z } = harness.flight.currentState();
      positions.push({ x, y, z });
    };
    for (let i = 0; i <= 5; i += 1) {
      harness.frame(i * FRAME_MS);
      record();
    }
    aim = own({ ...TO, w: CARD_W, h: CARD_H, angleDeg: 0 });
    harness.frame(6 * FRAME_MS);
    record();

    const step = (a: { x: number; y: number; z: number }, b: { x: number; y: number; z: number }) =>
      Math.hypot(a.x - b.x, a.y - b.y, a.z - b.z);
    const legStep = step(positions[5], positions[4]);
    expect(legStep).toBeGreaterThan(0);
    expect(step(positions[6], positions[5])).toBeLessThan(legStep);

    // The rebased leg lasts at least 60% of the flight; nothing releases before it ends.
    let ms = 6 * FRAME_MS;
    while (ms < 6 * FRAME_MS + 0.6 * RESOLVE_FLIGHT_MS - FRAME_MS) {
      ms += FRAME_MS;
      harness.frame(ms);
    }
    expect(harness.releases).toEqual([]);
    for (let i = 0; i < 60 && harness.releases.length === 0; i += 1) {
      ms += FRAME_MS;
      harness.frame(ms);
    }
    expect(harness.releases).toEqual(["land"]);
  });

  it("V3-6d (i): a leg that ends on a provisional node follows it and lands only once it becomes the own node", () => {
    const pile = document.createElement("div");
    let kind: "provisional" | "own" = "provisional";
    const slot = { ...TO, w: CARD_W, h: CARD_H };
    const harness = fly({ from: "Stack", to: "Graveyard", ownerId: 1 }, {
      aim: () =>
        kind === "own"
          ? { kind, el: pile, pose: { ...slot }, opacity: 1, faceImagesSettled: true }
          : { kind, el: pile, pose: { ...slot } },
    });
    let ms = 0;
    for (; ms <= RESOLVE_FLIGHT_MS + LAND_STATIONARY_WAIT_MAX_MS + 4 * FRAME_MS; ms += FRAME_MS) harness.frame(ms);
    expect(harness.releases).toEqual([]);
    expect(harness.flight.currentState().x).toBeCloseTo(slot.x, 6);

    // The pile now lists the object: the same node is its own node. A
    // provisional aim carries no opacity, so the first own frame has no
    // baseline and the card lands on the next one.
    kind = "own";
    harness.frame(ms);
    expect(harness.releases).toEqual([]);
    harness.frame(ms + FRAME_MS);
    expect(harness.releases).toEqual(["land"]);
  });

  it("V3-6d (i): a hold that reaches its end keeps hovering rather than landing", () => {
    const harness = fly({ from: "Stack", to: "Battlefield", ownerId: 0 }, { aim: () => ({ kind: "hold" }) });
    for (let ms = 0; ms <= RESOLVE_FLIGHT_MS + LAND_STATIONARY_WAIT_MAX_MS + 4 * FRAME_MS; ms += FRAME_MS) {
      expect(harness.frame(ms)).toBe(true);
    }
    expect(harness.releases).toEqual([]);
    expect(harness.flight.currentState().y).toBeCloseTo(FROM.y - FROM.h * HOLD_RISE_FRACTION, 6);
  });

  it("V3-6d (ii): after the commit, an own node missing for two frames abandons and fades out", () => {
    let committed = false;
    const pace = 1.5;
    const harness = fly({ from: "Stack", to: "Battlefield", ownerId: 0 }, { aim: () => ({ kind: "hold" }), committed: () => committed, pace });
    harness.frame(0);
    harness.frame(FRAME_MS);
    expect(harness.releases).toEqual([]);

    committed = true;
    let ms = FRAME_MS;
    for (let i = 1; i < ABSENT_FRAMES_AFTER_COMMIT; i += 1) {
      ms += FRAME_MS;
      harness.frame(ms);
    }
    expect(harness.releases).toEqual([]);
    ms += FRAME_MS;
    harness.frame(ms);
    expect(harness.releases).toEqual(["abandon"]);

    const fadeMs = ABANDON_FADE_MS * pace;
    expect(harness.frame(ms + fadeMs / 2)).toBe(true);
    expect(harness.alpha()).toBeCloseTo(0.5, 2);
    expect(harness.frame(ms + fadeMs)).toBe(false);
    expect(harness.releases).toEqual(["abandon"]);
  });

  it("V3-6d (iii): with no commit, the flight abandons at the await bound", () => {
    const pace = 0.5;
    const harness = fly({ from: "Stack", to: "Battlefield", ownerId: 0 }, { aim: () => ({ kind: "hold" }), pace });
    harness.frame(0);
    harness.frame(CARD_FLIGHT_MAX_AWAIT_MS * pace - 1);
    expect(harness.releases).toEqual([]);
    harness.frame(CARD_FLIGHT_MAX_AWAIT_MS * pace);
    expect(harness.releases).toEqual(["abandon"]);
  });

  it("V3-6d (iv): after the commit, a present own node is landed on, not abandoned", () => {
    let committed = false;
    const slot = own({ ...TO, w: CARD_W, h: CARD_H });
    const harness = fly({ from: "Stack", to: "Battlefield", ownerId: 0 }, { aim: () => slot, committed: () => committed });
    harness.frame(0);
    committed = true;
    for (let ms = FRAME_MS; ms <= RESOLVE_FLIGHT_MS + SETTLE_MS + 2 * FRAME_MS; ms += FRAME_MS) harness.frame(ms);
    expect(harness.releases).toEqual(["land"]);
  });

  it("V3-6e: a moving own node is followed, and landed on once it stops or the wait runs out", () => {
    const target = { ...TO, w: CARD_W, h: CARD_H };
    const el = document.createElement("div");
    let moving = true;
    const harness = fly({ from: "Hand", to: "Stack", ownerId: 0 }, {
      aim: () => {
        if (moving) target.x += 10;
        return { kind: "own", el, pose: { ...target }, opacity: 1, faceImagesSettled: true };
      },
    });
    let ms = 0;
    for (; ms <= CAST_FLIGHT_MS + 3 * FRAME_MS; ms += FRAME_MS) harness.frame(ms);
    expect(harness.releases).toEqual([]);
    expect(harness.flight.currentState().x).toBeCloseTo(target.x, 6);

    moving = false;
    harness.frame(ms);
    expect(harness.releases).toEqual(["land"]);

    // Still moving: the wait bound lands it anyway.
    const drifting = { ...TO, w: CARD_W, h: CARD_H };
    const pace = 2;
    const second = fly({ from: "Hand", to: "Stack", ownerId: 0 }, {
      aim: () => {
        drifting.x += 10;
        return { kind: "own", el, pose: { ...drifting }, opacity: 1, faceImagesSettled: true };
      },
      pace,
    });
    const arrival = CAST_FLIGHT_MS * pace;
    for (let t = 0; t < arrival; t += FRAME_MS) second.frame(t);
    second.frame(arrival);
    second.frame(arrival + LAND_STATIONARY_WAIT_MAX_MS * pace - 1);
    expect(second.releases).toEqual([]);
    second.frame(arrival + LAND_STATIONARY_WAIT_MAX_MS * pace);
    expect(second.releases).toEqual(["land"]);
  });

  it("V3-6f: a card-shaped slot settles opaque before release; any other shape releases at once and fades", () => {
    const cardSlot = own({ ...TO, w: CARD_W, h: CARD_H });
    const card = fly({ from: "Stack", to: "Battlefield", ownerId: 0 }, { aim: () => cardSlot });
    card.frame(0);
    card.frame(RESOLVE_FLIGHT_MS);
    expect(card.releases).toEqual([]);
    card.frame(RESOLVE_FLIGHT_MS + SETTLE_MS - 1);
    expect(card.releases).toEqual([]);
    card.frame(RESOLVE_FLIGHT_MS + SETTLE_MS);
    expect(card.releases).toEqual(["land"]);
    expect(card.alpha()).toBe(1);
    expect(card.frame(RESOLVE_FLIGHT_MS + SETTLE_MS + FRAME_MS)).toBe(false);

    // Any other shape releases at once; the card stays opaque until the slot
    // shows its face, then fades out over it.
    const state = { settled: false };
    const squareEl = document.createElement("div");
    const square = fly({ from: "Stack", to: "Battlefield", ownerId: 0 }, {
      aim: () => ({
        kind: "own",
        el: squareEl,
        pose: { ...TO, w: 100, h: 100 },
        opacity: 1,
        faceImagesSettled: state.settled,
      }),
    });
    square.frame(0);
    square.frame(RESOLVE_FLIGHT_MS);
    expect(square.releases).toEqual(["land"]);
    expect(square.alpha()).toBe(1);
    for (let i = 1; i <= 3; i += 1) {
      expect(square.frame(RESOLVE_FLIGHT_MS + i * FRAME_MS)).toBe(true);
      expect(square.alpha()).toBe(1);
    }
    state.settled = true;
    const settledMs = RESOLVE_FLIGHT_MS + 4 * FRAME_MS;
    expect(square.frame(settledMs)).toBe(true);
    expect(square.alpha()).toBe(1);
    expect(square.frame(settledMs + LANDING_CROSSFADE_MS / 2)).toBe(true);
    expect(square.alpha()).toBeCloseTo(0.5, 2);
    expect(square.frame(settledMs + LANDING_CROSSFADE_MS)).toBe(false);
    // The GL card keeps the card aspect inside the square slot.
    const { w, h } = square.flight.currentState();
    expect(w / h).toBeCloseTo(CARD_ASPECT, 6);
  });
  it("V3-6e: an own node's opacity joins the stationary test", () => {
    const el = document.createElement("div");
    const slot = { ...TO, w: CARD_W, h: CARD_H };
    const state = { opacity: 0.5, step: 0.013 };
    const ownAim = (): Aim => {
      state.opacity += state.step;
      return { kind: "own", el, pose: { ...slot }, opacity: state.opacity, faceImagesSettled: true };
    };
    // Geometry is still but the opacity is rising: an entrance tail, no release.
    const rising = fly({ from: "Hand", to: "Stack", ownerId: 0 }, { aim: ownAim });
    let ms = 0;
    for (; ms <= CAST_FLIGHT_MS + 3 * FRAME_MS; ms += FRAME_MS) rising.frame(ms);
    expect(rising.releases).toEqual([]);
    // Once it stops changing, it lands on that frame.
    state.step = 0;
    rising.frame(ms);
    expect(rising.releases).toEqual(["land"]);

    // A steady translucent node lands on its first stationary frame.
    const steady = fly({ from: "Hand", to: "Stack", ownerId: 0 }, {
      aim: () => ({ kind: "own", el, pose: { ...slot }, opacity: 0.5, faceImagesSettled: true }),
    });
    let landedAt: number | null = null;
    for (let t = 0; t < 2 * CAST_FLIGHT_MS && landedAt === null; t += FRAME_MS) {
      steady.frame(t);
      if (steady.releases.length > 0) landedAt = t;
    }
    expect(landedAt).toBe(Math.ceil(CAST_FLIGHT_MS / FRAME_MS) * FRAME_MS);

    // An opacity that never stops changing: the wait bound lands it anyway.
    let drift = 0.3;
    const changing = fly({ from: "Hand", to: "Stack", ownerId: 0 }, {
      aim: () => {
        drift += 0.01;
        return { kind: "own", el, pose: { ...slot }, opacity: drift, faceImagesSettled: true };
      },
    });
    for (let t = 0; t < CAST_FLIGHT_MS; t += FRAME_MS) changing.frame(t);
    changing.frame(CAST_FLIGHT_MS);
    changing.frame(CAST_FLIGHT_MS + LAND_STATIONARY_WAIT_MAX_MS - 1);
    expect(changing.releases).toEqual([]);
    changing.frame(CAST_FLIGHT_MS + LAND_STATIONARY_WAIT_MAX_MS);
    expect(changing.releases).toEqual(["land"]);
  });
});

/** Flies onto a shared own-node state until the land release and returns the
 *  harness, the release frame's time and the state the aim reads. */
function landOn(route: CardFlightRoute, slot: CardPose, { pace = 1 }: { pace?: number } = {}) {
  const state = { pose: slot, opacity: 1, settled: true, present: true };
  const el = document.createElement("div");
  const harness = fly(route, {
    aim: () =>
      state.present
        ? { kind: "own", el, pose: { ...state.pose }, opacity: state.opacity, faceImagesSettled: state.settled }
        : { kind: "hold" },
    pace,
  });
  let ms = 0;
  while (harness.releases.length === 0) {
    harness.frame(ms);
    if (harness.releases.length > 0) break;
    ms += FRAME_MS;
    expect(ms).toBeLessThan(5000);
  }
  return { harness, state, releasedAt: ms };
}

const CARD_SLOT: CardPose = { ...TO, w: CARD_W, h: CARD_H };
const SQUARE_SLOT: CardPose = { ...TO, w: 100, h: 100 };

describe("card flight reveal hold", () => {
  it.each<CardFlightRoute>([{ from: "Hand", to: "Stack", ownerId: 0 }, { from: "Stack", to: "Battlefield", ownerId: 0 }])(
    "V3-6g: the %o profile keeps drawing the card opaque until the face settles",
    (route) => {
      const { harness, state, releasedAt } = landOn(route, CARD_SLOT);
      // The release frame reads the pre-unveil content; then the face is a pulse.
      state.settled = false;
      for (let i = 1; i <= 3; i += 1) {
        expect(harness.frame(releasedAt + i * FRAME_MS)).toBe(true);
        expect(harness.alpha()).toBe(1);
        const { x, y } = harness.flight.currentState();
        expect(x).toBeCloseTo(CARD_SLOT.x, 6);
        expect(y).toBeCloseTo(CARD_SLOT.y, 6);
      }
      state.settled = true;
      expect(harness.frame(releasedAt + 4 * FRAME_MS)).toBe(false);
    },
  );

  it("V3-6g: a node already settled after release disposes on the first frame after it", () => {
    const { harness, releasedAt } = landOn({ from: "Hand", to: "Stack", ownerId: 0 }, CARD_SLOT);
    expect(harness.frame(releasedAt + FRAME_MS)).toBe(false);
  });

  it.each([1, 1.5])("V3-6h: an unsettled face is held for the fixed bound at pace %d", (pace) => {
    const { harness, state, releasedAt } = landOn({ from: "Hand", to: "Stack", ownerId: 0 }, CARD_SLOT, { pace });
    state.settled = false;
    expect(harness.frame(releasedAt + LAND_REVEAL_WAIT_MAX_MS - 1)).toBe(true);
    expect(harness.frame(releasedAt + LAND_REVEAL_WAIT_MAX_MS)).toBe(false);

    const square = landOn({ from: "Hand", to: "Stack", ownerId: 0 }, SQUARE_SLOT, { pace });
    square.state.settled = false;
    expect(square.harness.frame(square.releasedAt + LAND_REVEAL_WAIT_MAX_MS - 1)).toBe(true);
    expect(square.harness.alpha()).toBe(1);
    // The bound starts the cross-fade rather than ending the card.
    expect(square.harness.frame(square.releasedAt + LAND_REVEAL_WAIT_MAX_MS)).toBe(true);
    expect(square.harness.alpha()).toBe(1);
    const fadeMs = LANDING_CROSSFADE_MS * pace;
    expect(square.harness.frame(square.releasedAt + LAND_REVEAL_WAIT_MAX_MS + fadeMs / 2)).toBe(true);
    expect(square.harness.alpha()).toBeCloseTo(0.5, 2);
    expect(square.harness.frame(square.releasedAt + LAND_REVEAL_WAIT_MAX_MS + fadeMs)).toBe(false);
  });

  it("V3-6i: the card follows a moving node during the hold and leaves when the node is gone", () => {
    const { harness, state, releasedAt } = landOn({ from: "Hand", to: "Stack", ownerId: 0 }, CARD_SLOT);
    state.settled = false;
    state.pose = { ...CARD_SLOT, x: CARD_SLOT.x + 10 };
    expect(harness.frame(releasedAt + FRAME_MS)).toBe(true);
    expect(harness.flight.currentState().x).toBeCloseTo(CARD_SLOT.x + 10, 6);
    state.present = false;
    expect(harness.frame(releasedAt + 2 * FRAME_MS)).toBe(false);
  });

  it("V3-6j: a node that fades out ends the hold, measured against the first revealing frame", () => {
    const run = (route: CardFlightRoute, slot: CardPose, opacities: number[]) => {
      const { harness, state, releasedAt } = landOn(route, slot);
      state.settled = false;
      const results = opacities.map((opacity, i) => {
        state.opacity = opacity;
        return harness.frame(releasedAt + (i + 1) * FRAME_MS);
      });
      return { harness, results, releasedAt };
    };
    // A fall ends the hold on the frame it happens.
    expect(run({ from: "Hand", to: "Stack", ownerId: 0 }, CARD_SLOT, [1, 1, 0.95]).results).toEqual([true, true, false]);
    // The cross-fade path enters its fade on that frame.
    const fading = run({ from: "Hand", to: "Stack", ownerId: 0 }, SQUARE_SLOT, [1, 1, 0.95]);
    expect(fading.results).toEqual([true, true, true]);
    expect(fading.harness.frame(fading.releasedAt + 3 * FRAME_MS + LANDING_CROSSFADE_MS / 2)).toBe(true);
    expect(fading.harness.alpha()).toBeCloseTo(0.5, 2);
    // A steady node, a rising tail and a fall below the bound keep the hold.
    expect(run({ from: "Hand", to: "Stack", ownerId: 0 }, CARD_SLOT, [0.5, 0.5, 0.5, 0.5]).results).toEqual([true, true, true, true]);
    expect(run({ from: "Hand", to: "Stack", ownerId: 0 }, CARD_SLOT, [0.96, 0.98, 1]).results).toEqual([true, true, true]);
    expect(run({ from: "Hand", to: "Stack", ownerId: 0 }, CARD_SLOT, [1, 0.999]).results).toEqual([true, true]);
    // A slow fade, each frame below the bound, still ends it once the total fall
    // passes the bound from the first revealing frame's opacity.
    expect(run({ from: "Hand", to: "Stack", ownerId: 0 }, CARD_SLOT, [0.9985, 0.997, 0.9955]).results).toEqual([true, true, false]);
  });
});
