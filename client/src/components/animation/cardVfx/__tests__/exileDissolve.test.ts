import { type Points, Scene, type ShaderMaterial, Texture } from "three";
import { describe, expect, it, vi } from "vitest";

import type { EffectHost } from "../cardVfxScene.ts";
import {
  createExileDissolve,
  DISSOLVE_CROSS_S,
  DISSOLVE_LIFT_S,
  DISSOLVE_TRAVEL_S,
  type ExileDissolveParams,
  exileDissolveKind,
  type LinkAim,
} from "../exileDissolve.ts";

function host(): EffectHost {
  return {
    scene: new Scene(),
    backTexture: null,
    placeholderTexture: new Texture(),
    canvasOrigin: () => new DOMRectReadOnly(0, 0, 800, 600),
  };
}

const POSE = { x: 300, y: 400, w: 63, h: 88, angleDeg: 0 };

function params(overrides: Partial<ExileDissolveParams> = {}): ExileDissolveParams {
  return {
    pose: POSE,
    surface: new Texture(),
    radius: 4,
    link: null,
    tier: "full",
    pace: 1,
    pixelRatio: 1,
    onArrive: vi.fn(),
    ...overrides,
  };
}

const flakeUniforms = (scene: Scene) =>
  (scene.getObjectByName("exile-dissolve-flakes") as Points<never, ShaderMaterial>).material.uniforms;

describe("exile dissolve", () => {
  it("V9-1: out of the game, the card lifts, dissolves and drifts away, arriving only as it ends", () => {
    const effectHost = host();
    const onArrive = vi.fn();
    const dissolve = createExileDissolve(effectHost, params({ pace: 2, onArrive }));
    expect(effectHost.scene.children.map((child) => child.name)).toEqual([
      "exile-dissolve",
      "exile-dissolve-shadow",
      "exile-dissolve-flakes",
    ]);
    expect(flakeUniforms(effectHost.scene).uHeld.value).toBe(0);

    const totalMs = (DISSOLVE_LIFT_S + DISSOLVE_CROSS_S + DISSOLVE_TRAVEL_S * 1.3) * 1000 * 2;
    expect(dissolve.update(0)).toBe(true);
    expect(dissolve.update(totalMs - 20)).toBe(true);
    expect(onArrive).not.toHaveBeenCalled();
    expect(dissolve.update(totalMs + 20)).toBe(false);

    dissolve.dispose(false);
    expect(effectHost.scene.children).toHaveLength(0);
    expect(onArrive).toHaveBeenCalledTimes(1);
  });

  it("V9-2: a held card's flakes aim at the holder until its ghost exists, and arrive before the effect ends", () => {
    const effectHost = host();
    const onArrive = vi.fn();
    const holder = { x: 500, y: 400, w: 63, h: 88, angleDeg: 0 };
    let aim: LinkAim = { ghost: null, holder };
    const dissolve = createExileDissolve(effectHost, params({ link: () => aim, onArrive }));
    const uniforms = flakeUniforms(effectHost.scene);
    expect(uniforms.uHeld.value).toBe(1);
    // The side facing the holder dissolves first.
    expect(uniforms.uFirst.value.toArray()).toEqual([1, -0]);

    dissolve.update(0);
    expect(uniforms.uGhostPos.value.toArray()).toEqual([500, -400, 0]);
    aim = { ghost: { x: 510, y: 420, w: 63, h: 88, angleDeg: 0 }, holder };
    dissolve.update(100);
    expect(uniforms.uGhostPos.value.toArray()).toEqual([510, -420, 0]);
    expect(uniforms.uHolder.value.toArray()).toEqual([500, -400, 31.5, 44]);

    const arrivalMs = (DISSOLVE_LIFT_S + DISSOLVE_CROSS_S + DISSOLVE_TRAVEL_S * 0.85) * 1000;
    dissolve.update(arrivalMs + 20);
    expect(onArrive).toHaveBeenCalledTimes(1);
    expect(dissolve.update(arrivalMs + 40)).toBe(true);
    dissolve.dispose(false);
    expect(onArrive).toHaveBeenCalledTimes(1);
  });

  it("V9-3: reduced has no shadow; a silent dispose reports nothing", () => {
    const effectHost = host();
    const onArrive = vi.fn();
    createExileDissolve(effectHost, params({ tier: "reduced", onArrive })).dispose(true);
    expect(onArrive).not.toHaveBeenCalled();

    createExileDissolve(effectHost, params({ tier: "reduced" }));
    expect(effectHost.scene.getObjectByName("exile-dissolve-shadow")).toBeUndefined();
  });

  it("V9-4: the warm-up declares the card, shadow and flake passes", () => {
    expect(exileDissolveKind.warmUp(host()).map((object) => object.name)).toEqual([
      "exile-dissolve-warmup",
      "exile-dissolve-shadow-warmup",
      "exile-dissolve-flakes-warmup",
    ]);
  });
});
