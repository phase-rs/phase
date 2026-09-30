import { act, cleanup, render } from "@testing-library/react";
import type { RefObject } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import type { AnimationStep } from "../../../animation/types.ts";
import { currentSnapshot } from "../../../hooks/useGameDispatch.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { buildObjectMap, gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { AnimationOverlay } from "../AnimationOverlay.tsx";
import { CardRevealBurst } from "../CardRevealBurst.tsx";
import { CastArcAnimation } from "../CastArcAnimation.tsx";
import type { CardFlightSpec } from "../cardVfx/cardFlightSpecs.ts";
import type { CardVfxLayerHandle } from "../cardVfx/CardVfxLayer.tsx";
import type { ParticleCanvasHandle } from "../ParticleCanvas.tsx";

const layer = vi.hoisted(() => ({
  supported: undefined as boolean | undefined,
  present: vi.fn<(spec: CardFlightSpec, classic: () => void) => void>(),
}));

vi.mock("../cardVfx/CardVfxLayer.tsx", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../cardVfx/CardVfxLayer.tsx")>();
  const { createElement, forwardRef, useImperativeHandle } = await import("react");
  return {
    ...actual,
    cardVfxSupported: () => layer.supported ?? actual.cardVfxSupported(),
    CardVfxLayer: forwardRef<CardVfxLayerHandle>((_props, ref) => {
      useImperativeHandle(ref, () => ({ present: layer.present }));
      return createElement("canvas", { "data-card-vfx": "" });
    }),
  };
});

vi.mock("../CastArcAnimation.tsx", () => ({ CastArcAnimation: vi.fn(() => null) }));
vi.mock("../CardRevealBurst.tsx", () => ({ CardRevealBurst: vi.fn(() => null) }));

const motion = vi.hoisted(() => ({ reduced: false }));

vi.mock("framer-motion", async (importOriginal) => ({
  ...(await importOriginal<typeof import("framer-motion")>()),
  useReducedMotion: () => motion.reduced,
}));

const particles = vi.hoisted(() => ({
  explosion: vi.fn(),
  projectile: vi.fn(),
  spellImpact: vi.fn(),
  damageFlash: vi.fn(),
  playerDamage: vi.fn(),
  healEffect: vi.fn(),
  summonBurst: vi.fn(),
  blockClash: vi.fn(),
  attackBurst: vi.fn(),
  slamImpact: vi.fn(),
  forgeStrike: vi.fn(),
  forgeHeat: vi.fn(),
  damageFlurry: vi.fn(),
}) satisfies ParticleCanvasHandle);

vi.mock("../ParticleCanvas.tsx", async () => {
  const { forwardRef, useImperativeHandle } = await import("react");
  return {
    ParticleCanvas: forwardRef<ParticleCanvasHandle>((_props, ref) => {
      useImperativeHandle(ref, () => particles);
      return null;
    }),
  };
});

const X = 7;
const containerRef = { current: null } as RefObject<HTMLDivElement | null>;
const elves = gameObjectFactory.withId(X).named("Llanowar Elves").creature(1, 1);

function stateWith(object: GameObject): GameState {
  return buildGameState({ objects: buildObjectMap({ ...object, display_visible_to_viewer: true }) });
}

function seed(event: AnimationStep["effects"][number]["event"], pre: GameObject, post: GameObject) {
  currentSnapshot.set(X, new DOMRect(40, 600, 63, 88));
  act(() => {
    useGameStore.setState({ gameState: stateWith(pre) });
    useAnimationStore.getState().setAnimationNewState(stateWith(post));
    useAnimationStore.getState().enqueueSteps([{ effects: [{ event, duration: 500 }], duration: 500 }]);
  });
}

const spellCast = { type: "SpellCast", data: { card_id: X, controller: 0, object_id: X } } as const;

function seedCast() {
  seed(spellCast, elves.inHand().build(), elves.params({ zone: "Stack" }).build());
}

function renderOverlay() {
  return render(<AnimationOverlay containerRef={containerRef} />);
}

function castArcs() {
  return vi.mocked(CastArcAnimation).mock.calls.map(([props]) => props);
}

const hardcodedStackPoint = () => ({ x: window.innerWidth * 0.75, y: window.innerHeight * 0.4 });

function expectClassicCast() {
  expect(castArcs()).toEqual([expect.objectContaining({ mode: "cast", to: hardcodedStackPoint() })]);
  expect(particles.spellImpact).toHaveBeenCalledTimes(1);
}

const overlayCanvas = () => document.querySelector("canvas[data-card-vfx]");

beforeEach(() => {
  currentSnapshot.clear();
  motion.reduced = false;
  layer.supported = undefined;
  usePreferencesStore.setState({
    cardAnimationStyle: "webgl",
    vfxQuality: "full",
    animationSpeedMultiplier: 1,
  });
});

afterEach(() => {
  cleanup();
  useAnimationStore.getState().clearQueue();
  useGameStore.getState().reset();
  currentSnapshot.clear();
  vi.clearAllMocks();
  vi.useRealTimers();
});

describe("AnimationOverlay card VFX seam", () => {
  it("V3-2a: the Classic style runs today's cast arc to the hardcoded point and never offers the layer", () => {
    usePreferencesStore.setState({ cardAnimationStyle: "classic" });
    layer.supported = true;
    seedCast();

    renderOverlay();

    expectClassicCast();
    expect(layer.present).not.toHaveBeenCalled();
    expect(overlayCanvas()).toBeNull();
  });

  it("V3-2b: the minimal tier mounts no layer and keeps today's minimal output", () => {
    usePreferencesStore.setState({ vfxQuality: "minimal" });
    layer.supported = true;
    seed(
      { type: "ZoneChanged", data: { object_id: X, from: "Stack", to: "Battlefield" } },
      elves.params({ zone: "Stack" }).build(),
      elves.onBattlefield().build(),
    );

    renderOverlay();

    expect(overlayCanvas()).toBeNull();
    expect(layer.present).not.toHaveBeenCalled();
    expect(CardRevealBurst).toHaveBeenCalled();
  });

  it("V3-2c: reduced motion under the New style runs the Classic cast arc and mounts no layer", () => {
    motion.reduced = true;
    layer.supported = true;
    seedCast();

    renderOverlay();

    expectClassicCast();
    expect(overlayCanvas()).toBeNull();
    expect(layer.present).not.toHaveBeenCalled();
  });

  it("V3-2d: without WebGL 2 (happy-dom) the default style runs Classic and mounts no layer", () => {
    seedCast();

    renderOverlay();

    expectClassicCast();
    expect(overlayCanvas()).toBeNull();
  });

  it("V3-2e: a mounted layer presents the cast instead of Classic, at the configured pace", () => {
    layer.supported = true;
    seedCast();

    renderOverlay();

    expect(overlayCanvas()).not.toBeNull();
    expect(layer.present).toHaveBeenCalledTimes(1);
    expect(layer.present.mock.calls[0][0]).toMatchObject({
      objectId: X,
      route: { from: "Hand", to: "Stack", ownerId: 0 },
      pace: 1,
      owningStepMs: 500,
    });
    expect(castArcs()).toEqual([]);
    expect(particles.spellImpact).not.toHaveBeenCalled();

    // The thunk it was handed is the Classic effect for this very event.
    act(() => layer.present.mock.calls[0][1]());
    expectClassicCast();
  });

  it("V3-2e: the pace follows the speed multiplier, and multiplier 0 runs Classic", () => {
    layer.supported = true;
    usePreferencesStore.setState({ animationSpeedMultiplier: 1.5 });
    seedCast();
    const { unmount } = renderOverlay();
    expect(layer.present.mock.calls[0][0]).toMatchObject({ pace: 1.5, owningStepMs: 750 });
    unmount();
    act(() => useAnimationStore.getState().clearQueue());
    vi.clearAllMocks();

    usePreferencesStore.setState({ animationSpeedMultiplier: 0 });
    seedCast();
    renderOverlay();
    expect(layer.present).not.toHaveBeenCalled();
    expectClassicCast();
  });
});

describe("AnimationOverlay step timing under both styles", () => {
  it.each(["webgl", "classic"] as const)("V3-7: a 500 ms cast step under %s advances at 500 ms, not before", (style) => {
    vi.useFakeTimers();
    usePreferencesStore.setState({ cardAnimationStyle: style });
    layer.supported = true;
    seedCast();

    renderOverlay();
    expect(useAnimationStore.getState().activeStep).not.toBeNull();
    if (style === "webgl") expect(layer.present).toHaveBeenCalledTimes(1);

    act(() => {
      vi.advanceTimersByTime(499);
    });
    expect(useAnimationStore.getState().activeStep).not.toBeNull();
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(useAnimationStore.getState().activeStep).toBeNull();
  });
});
