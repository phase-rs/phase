import { type Mesh, PerspectiveCamera, Scene, type ShaderMaterial, Texture, Vector3 } from "three";
import { describe, expect, it } from "vitest";

import type { Aim, CardPose } from "../cardAnchors.ts";
import {
  ABANDON_FADE_MS,
  ABSENT_FRAMES_AFTER_COMMIT,
  CARD_ASPECT,
  CARD_FLIGHT_MAX_AWAIT_MS,
  CAST_FLIGHT_MS,
  createCardFlight,
  flightPose,
  type FlightCurve,
  type FlightRelease,
  HOLD_RISE_FRACTION,
  LAND_STATIONARY_WAIT_MAX_MS,
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
  return { kind: "own", el: document.createElement("div"), pose };
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
  { aim, epoch = () => 0, pace = 1 }: { aim: () => Aim; epoch?: () => number; pace?: number },
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
    tier: "full",
    aim,
    commitEpoch: epoch,
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
    const harness = fly({ kind: "resolveToBattlefield" }, { aim: () => aim });
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
    const harness = fly({ kind: "resolveToGraveyard", ownerId: 1 }, {
      aim: () => ({ kind, el: pile, pose: { ...slot } }),
    });
    let ms = 0;
    for (; ms <= RESOLVE_FLIGHT_MS + LAND_STATIONARY_WAIT_MAX_MS + 4 * FRAME_MS; ms += FRAME_MS) harness.frame(ms);
    expect(harness.releases).toEqual([]);
    expect(harness.flight.currentState().x).toBeCloseTo(slot.x, 6);

    // The pile now lists the object: the same node is its own node.
    kind = "own";
    harness.frame(ms);
    expect(harness.releases).toEqual(["land"]);
  });

  it("V3-6d (i): a hold that reaches its end keeps hovering rather than landing", () => {
    const harness = fly({ kind: "resolveToBattlefield" }, { aim: () => ({ kind: "hold" }) });
    for (let ms = 0; ms <= RESOLVE_FLIGHT_MS + LAND_STATIONARY_WAIT_MAX_MS + 4 * FRAME_MS; ms += FRAME_MS) {
      expect(harness.frame(ms)).toBe(true);
    }
    expect(harness.releases).toEqual([]);
    expect(harness.flight.currentState().y).toBeCloseTo(FROM.y - FROM.h * HOLD_RISE_FRACTION, 6);
  });

  it("V3-6d (ii): after the commit, an own node missing for two frames abandons and fades out", () => {
    let epoch = 0;
    const pace = 1.5;
    const harness = fly({ kind: "resolveToBattlefield" }, { aim: () => ({ kind: "hold" }), epoch: () => epoch, pace });
    harness.frame(0);
    harness.frame(FRAME_MS);
    expect(harness.releases).toEqual([]);

    epoch = 1;
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
    const harness = fly({ kind: "resolveToBattlefield" }, { aim: () => ({ kind: "hold" }), pace });
    harness.frame(0);
    harness.frame(CARD_FLIGHT_MAX_AWAIT_MS * pace - 1);
    expect(harness.releases).toEqual([]);
    harness.frame(CARD_FLIGHT_MAX_AWAIT_MS * pace);
    expect(harness.releases).toEqual(["abandon"]);
  });

  it("V3-6d (iv): after the commit, a present own node is landed on, not abandoned", () => {
    let epoch = 0;
    const slot = own({ ...TO, w: CARD_W, h: CARD_H });
    const harness = fly({ kind: "resolveToBattlefield" }, { aim: () => slot, epoch: () => epoch });
    harness.frame(0);
    epoch = 1;
    for (let ms = FRAME_MS; ms <= RESOLVE_FLIGHT_MS + SETTLE_MS + 2 * FRAME_MS; ms += FRAME_MS) harness.frame(ms);
    expect(harness.releases).toEqual(["land"]);
  });

  it("V3-6e: a moving own node is followed, and landed on once it stops or the wait runs out", () => {
    const target = { ...TO, w: CARD_W, h: CARD_H };
    const el = document.createElement("div");
    let moving = true;
    const harness = fly({ kind: "cast" }, {
      aim: () => {
        if (moving) target.x += 10;
        return { kind: "own", el, pose: { ...target } };
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
    const second = fly({ kind: "cast" }, {
      aim: () => {
        drifting.x += 10;
        return { kind: "own", el, pose: { ...drifting } };
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
    const card = fly({ kind: "resolveToBattlefield" }, { aim: () => cardSlot });
    card.frame(0);
    card.frame(RESOLVE_FLIGHT_MS);
    expect(card.releases).toEqual([]);
    card.frame(RESOLVE_FLIGHT_MS + SETTLE_MS - 1);
    expect(card.releases).toEqual([]);
    card.frame(RESOLVE_FLIGHT_MS + SETTLE_MS);
    expect(card.releases).toEqual(["land"]);
    expect(card.alpha()).toBe(1);
    expect(card.frame(RESOLVE_FLIGHT_MS + SETTLE_MS + FRAME_MS)).toBe(false);

    const squareSlot = own({ ...TO, w: 100, h: 100 });
    const square = fly({ kind: "resolveToBattlefield" }, { aim: () => squareSlot });
    square.frame(0);
    square.frame(RESOLVE_FLIGHT_MS);
    expect(square.releases).toEqual(["land"]);
    square.frame(RESOLVE_FLIGHT_MS + ABANDON_FADE_MS / 2);
    expect(square.alpha()).toBeCloseTo(0.5, 2);
    // The GL card keeps the card aspect inside the square slot.
    const { w, h } = square.flight.currentState();
    expect(w / h).toBeCloseTo(CARD_ASPECT, 6);
  });
});
