import { Mesh, type Object3D, Scene, type ShaderMaterial, Texture } from "three";
import { describe, expect, it } from "vitest";

import { COUNTER_RIPPLE_MS, type RippleLook } from "../cardFlightSpecs.ts";
import type { EffectHost } from "../cardVfxScene.ts";
import { type CounterRippleParams, counterRippleKind, createCounterRipple } from "../counterRipple.ts";

function host(): EffectHost {
  return {
    scene: new Scene(),
    backTexture: null,
    placeholderTexture: new Texture(),
    canvasOrigin: () => new DOMRectReadOnly(0, 0, 800, 600),
  };
}

const FROM = { x: 700, y: 320, w: 150, h: 210, angleDeg: 0 };
const TO = { x: 700, y: 200, w: 150, h: 210, angleDeg: 0 };

function params(look: RippleLook, pace = 1): CounterRippleParams {
  return { from: FROM, to: TO, look, tier: "full", pace };
}

function meshes(effectHost: EffectHost) {
  const group = effectHost.scene.getObjectByName("counter-ripple") as Object3D;
  const found: Mesh<never, ShaderMaterial>[] = [];
  group.traverse((object) => {
    if (object instanceof Mesh) found.push(object as Mesh<never, ShaderMaterial>);
  });
  return found;
}

/** The visible ripple rings centred on the countered entry. */
function ringsOnTarget(effectHost: EffectHost) {
  return meshes(effectHost).filter(
    (mesh) => mesh.visible && "RIPPLE" in (mesh.material.defines ?? {}) && mesh.position.x === TO.x && mesh.position.y === -TO.y,
  );
}

describe("counter ripple", () => {
  it("V14-7: water throws a splash where it breaks; a pale ripple has no water in it", () => {
    const water = host();
    const pale = host();
    createCounterRipple(water, params("water"));
    createCounterRipple(pale, params("pale"));
    const drops = (effectHost: EffectHost) => meshes(effectHost).filter((mesh) => "DROP" in (mesh.material.defines ?? {}));
    expect(drops(water)).toHaveLength(1);
    expect(drops(pale)).toHaveLength(0);
  });

  it("V14-7: rings break over the countered spell only once the swell arrives, and the ripple ends on its paced clock", () => {
    const effectHost = host();
    const ripple = createCounterRipple(effectHost, params("pale", 2));
    const arrivalMs = COUNTER_RIPPLE_MS * 2;
    ripple.update(1000);
    ripple.update(1000 + arrivalMs - 20);
    expect(ringsOnTarget(effectHost)).toHaveLength(0);
    ripple.update(1000 + arrivalMs + 40);
    expect(ringsOnTarget(effectHost).length).toBeGreaterThan(0);
    expect(ripple.update(1000 + arrivalMs + 900 * 2 + 20)).toBe(false);
    ripple.dispose(false);
    expect(effectHost.scene.children).toHaveLength(0);
  });

  it("V14-7: the warm-up builds the ripple, drop and mist programs", () => {
    const warm = counterRippleKind.warmUp(host());
    const defines = warm.map((object) => Object.keys(((object as Mesh).material as ShaderMaterial).defines ?? {}));
    expect(defines).toEqual([["RIPPLE"], ["SMOKE"], ["DROP"]]);
  });
});
