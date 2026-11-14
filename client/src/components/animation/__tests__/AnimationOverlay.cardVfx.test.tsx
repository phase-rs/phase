import { act, cleanup, render } from "@testing-library/react";
import type { RefObject } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import {
  type AnimationEvent,
  type AnimationStep,
  CARD_SLAM_FLIGHT_MS,
  impactDelayMsForAnimationEvent,
} from "../../../animation/types.ts";
import { currentSnapshot } from "../../../hooks/useGameDispatch.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { buildObjectMap, gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildStackEntry } from "../../../test/factories/gameStateFactory.ts";
import { AnimationOverlay } from "../AnimationOverlay.tsx";
import { CardRevealBurst } from "../CardRevealBurst.tsx";
import { CARD_KNOCKBACK_MS } from "../CardSlamAnimation.tsx";
import { CastArcAnimation } from "../CastArcAnimation.tsx";
import type { CardVfxSpec } from "../cardVfx/cardVfxSpecs.ts";
import type { CardVfxLayerHandle } from "../cardVfx/CardVfxLayer.tsx";
import type { ParticleCanvasHandle } from "../ParticleCanvas.tsx";

const layer = vi.hoisted(() => ({
  supported: undefined as boolean | undefined,
  present: vi.fn<(spec: CardVfxSpec, classic: () => void) => void>(),
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
    useAnimationStore.getState().enqueueSteps([{ effects: [{ event, duration: 500 }], duration: 500 }], 1);
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
  useAnimationStore.getState().setCardVfxReady(false);
  layer.present.mockReset();
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

describe("AnimationOverlay announced casts", () => {
  function seedAnnouncedCast() {
    seedCast();
    const pre = useGameStore.getState().gameState as GameState;
    act(() => {
      useGameStore.setState({
        gameState: { ...pre, stack: [buildStackEntry({ id: X, source_id: X })] },
      });
    });
  }

  it("V10-12: a spell announced under a ready layer bursts on the stack at its cast without flying again", () => {
    layer.supported = true;
    layer.present.mockImplementation((_spec, classic) => classic());
    useAnimationStore.getState().setCardVfxReady(true);
    seedAnnouncedCast();

    renderOverlay();

    expect(castArcs()).toEqual([]);
    expect(particles.spellImpact).toHaveBeenCalledTimes(1);
  });

  it("V10-12: without a ready layer the announced cast keeps its Classic arc", () => {
    usePreferencesStore.setState({ cardAnimationStyle: "classic" });
    seedAnnouncedCast();

    renderOverlay();

    expectClassicCast();
  });
});

describe("AnimationOverlay combat blows", () => {
  const Y = 8;
  const hit = {
    type: "DamageDealt",
    data: { source_id: X, target: { Object: Y }, amount: 3, is_combat: true },
  } as const;

  function card(objectId: number, left: number) {
    const el = document.createElement("div");
    el.setAttribute("data-object-id", String(objectId));
    el.getBoundingClientRect = () => new DOMRect(left, 300, 63, 88);
    document.body.appendChild(el);
    return el;
  }

  function seedHit() {
    const bears = gameObjectFactory
      .withId(Y)
      .named("Grizzly Bears")
      .creature(2, 2)
      .onBattlefield()
      .params({ display_visible_to_viewer: true })
      .build();
    act(() => {
      useGameStore.setState({ gameState: buildGameState({ objects: buildObjectMap(elves.onBattlefield().build(), bears) }) });
      useAnimationStore.getState().enqueueSteps([{ effects: [{ event: hit, duration: 500 }], duration: 500 }], 1);
    });
  }

  const advance = (ms: number) =>
    act(() => {
      vi.advanceTimersByTime(ms);
    });

  beforeEach(() => {
    vi.useFakeTimers({
      toFake: ["setTimeout", "clearTimeout", "requestAnimationFrame", "cancelAnimationFrame", "performance"],
    });
  });

  afterEach(() => {
    document.body.replaceChildren();
  });

  it("V12-4: under the New style a slam's impact is the layer's blow, and the struck card's copy rocks back", () => {
    layer.supported = true;
    card(X, 40);
    const struck = card(Y, 400);
    seedHit();

    renderOverlay();

    const slam = { amount: 3, pace: 1, startMs: expect.any(Number), impactDelayMs: CARD_SLAM_FLIGHT_MS };
    expect(layer.present.mock.calls.map(([spec]) => spec)).toEqual([
      { kind: "blow", sourceId: X, target: { Object: Y }, ...slam },
      {
        kind: "knockback",
        objectId: Y,
        face: expect.objectContaining({ objectId: Y, cardName: "Grizzly Bears" }),
        sourceId: X,
        owningStepMs: 500,
        ...slam,
      },
    ]);
    advance(CARD_SLAM_FLIGHT_MS + 60);
    expect(particles.slamImpact).not.toHaveBeenCalled();
    // The layer's copy rocks back in its place; the real card stays still.
    expect(struck.style.rotate || "").toBe("");
  });

  it("V12-4: a blow the layer cannot present runs the Classic particles, and the real card rocks back, as the slam lands", () => {
    layer.supported = true;
    layer.present.mockImplementation((_spec, classic) => classic());
    card(X, 40);
    const struck = card(Y, 400);
    seedHit();

    renderOverlay();

    advance(CARD_SLAM_FLIGHT_MS - 40);
    expect(particles.slamImpact).not.toHaveBeenCalled();
    expect(struck.style.rotate || "").toBe("");
    advance(80);
    expect(particles.slamImpact).toHaveBeenCalledTimes(1);
    expect(struck.style.rotate).not.toBe("");
    advance(CARD_KNOCKBACK_MS + 100);
    expect(struck.style.rotate).toBe("");
  });

  it("V16-5: a knockback the layer gives up after the slam has landed rocks the real card back then", () => {
    layer.supported = true;
    let giveUp = () => {};
    layer.present.mockImplementation((spec, classic) => {
      if (spec.kind === "knockback") giveUp = classic;
    });
    card(X, 40);
    const struck = card(Y, 400);
    seedHit();

    renderOverlay();

    advance(CARD_SLAM_FLIGHT_MS + 60);
    expect(struck.style.rotate || "").toBe("");
    act(() => giveUp());
    advance(60);
    expect(struck.style.rotate).not.toBe("");
  });

  it("V12-4: a flurry of hits on a player lands one blow with no one direction, else its Classic burst", () => {
    layer.supported = true;
    layer.present.mockImplementation((_spec, classic) => classic());
    const flurry: AnimationEvent = {
      type: "GroupedDamageFlurry",
      data: { player_id: 1, source_ids: [X], total_damage: 9, hit_count: 3 },
    };
    act(() => {
      useGameStore.setState({ gameState: buildGameState({ objects: buildObjectMap(elves.onBattlefield().build()) }) });
      useAnimationStore.getState().enqueueSteps([{ effects: [{ event: flurry, duration: 500 }], duration: 500 }], 1);
    });

    renderOverlay();

    const impactDelayMs = impactDelayMsForAnimationEvent(flurry);
    expect(layer.present.mock.calls.map(([spec]) => spec)).toEqual([
      { kind: "blow", sourceId: null, target: { Player: 1 }, amount: 9, pace: 1, startMs: expect.any(Number), impactDelayMs },
    ]);
    advance(impactDelayMs + 10);
    expect(particles.playerDamage).toHaveBeenCalledTimes(1);
  });

  it("V12-4: the Classic style keeps today's slam: particles at the impact, no blow and no knockback", () => {
    usePreferencesStore.setState({ cardAnimationStyle: "classic" });
    card(X, 40);
    const struck = card(Y, 400);
    seedHit();

    renderOverlay();

    advance(CARD_SLAM_FLIGHT_MS + 60);
    expect(layer.present).not.toHaveBeenCalled();
    expect(particles.slamImpact).toHaveBeenCalledTimes(1);
    expect(struck.style.rotate).toBeFalsy();
  });
});

describe("AnimationOverlay life changes", () => {
  function seedLife(effects: AnimationStep["effects"]) {
    act(() => {
      useGameStore.setState({ gameState: buildGameState({ objects: buildObjectMap(elves.onBattlefield().build()) }) });
      useAnimationStore.getState().enqueueSteps([{ effects, duration: 500 }], 1);
    });
  }

  const gained = { type: "LifeChanged", data: { player_id: 1, amount: 3 } } as const;

  it("V13-5: under the New style life gained is the layer's, with the heal particles as its Classic", () => {
    layer.supported = true;
    seedLife([{ event: gained, duration: 500 }]);

    renderOverlay();

    expect(layer.present.mock.calls.map(([spec]) => spec)).toEqual([{ kind: "life", playerId: 1, amount: 3, pace: 1 }]);
    expect(particles.healEffect).not.toHaveBeenCalled();
    act(() => layer.present.mock.calls[0][1]());
    expect(particles.healEffect).toHaveBeenCalledTimes(1);
  });

  it("V13-5: a loss damage already shows is not presented again", () => {
    layer.supported = true;
    const hit = { type: "DamageDealt", data: { source_id: X, target: { Player: 1 }, amount: 2, is_combat: false } } as const;
    const lost = { type: "LifeChanged", data: { player_id: 1, amount: -2 } } as const;
    seedLife([
      { event: hit, duration: 500 },
      { event: lost, duration: 500 },
    ]);

    renderOverlay();

    expect(layer.present.mock.calls.map(([spec]) => spec.kind)).not.toContain("life");
  });

  it("V13-5: a gain in a step where damage hits the same player is still the layer's", () => {
    layer.supported = true;
    const hit = { type: "DamageDealt", data: { source_id: X, target: { Player: 1 }, amount: 2, is_combat: false } } as const;
    seedLife([
      { event: hit, duration: 500 },
      { event: gained, duration: 500 },
    ]);

    renderOverlay();

    expect(layer.present.mock.calls.map(([spec]) => spec).filter((spec) => spec.kind === "life")).toEqual([
      { kind: "life", playerId: 1, amount: 3, pace: 1 },
    ]);
  });

  it("V13-5: the Classic style keeps the heal particles", () => {
    usePreferencesStore.setState({ cardAnimationStyle: "classic" });
    seedLife([{ event: gained, duration: 500 }]);

    renderOverlay();

    expect(layer.present).not.toHaveBeenCalled();
    expect(particles.healEffect).toHaveBeenCalledTimes(1);
  });
});
