import { type InstancedBufferGeometry, Mesh, type Object3D, Scene, type ShaderMaterial, Texture } from "three";
import { describe, expect, it } from "vitest";

import type { EffectHost } from "../cardVfxScene.ts";
import {
  COUNTER_CHANGE_S,
  type CounterChangeParams,
  counterTint,
  createCounterChange,
  createLifeChange,
  LIFE_CHANGE_S,
} from "../tallyEffects.ts";

function host(): EffectHost {
  return {
    scene: new Scene(),
    backTexture: null,
    placeholderTexture: new Texture(),
    canvasOrigin: () => new DOMRectReadOnly(0, 0, 800, 600),
  };
}

const HUD = { x: 400, y: 40, w: 220, h: 48, angleDeg: 0 };
const CARD = { x: 300, y: 400, w: 63, h: 88, angleDeg: 0 };

function meshes(scene: Scene): Mesh[] {
  const found: Mesh[] = [];
  scene.traverse((object: Object3D) => {
    if (object instanceof Mesh) found.push(object);
  });
  return found;
}

const programs = (scene: Scene) =>
  meshes(scene)
    .map((mesh) => Object.keys((mesh.material as ShaderMaterial).defines ?? {}).join() || "GLOW")
    .sort();

const particleCount = (scene: Scene) =>
  meshes(scene).reduce((sum, mesh) => {
    const geometry = mesh.geometry as InstancedBufferGeometry;
    return sum + (geometry.isInstancedBufferGeometry ? geometry.instanceCount : 0);
  }, 0);

describe("life change", () => {
  it("V13-1: life gained rises as warm motes in a soft light; life lost sinks as dark wisps with none", () => {
    const gain = host();
    createLifeChange(gain, { at: HUD, amount: 3, tier: "full", pace: 1 });
    expect(programs(gain.scene)).toEqual(["FLAME", "GLOW"]);

    const loss = host();
    createLifeChange(loss, { at: HUD, amount: -3, tier: "full", pace: 1 });
    expect(programs(loss.scene)).toEqual(["SMOKE"]);
    const velocity = (meshes(loss.scene)[0].geometry as InstancedBufferGeometry).getAttribute("aVel");
    for (let i = 0; i < velocity.count; i += 1) expect(velocity.getY(i)).toBeLessThan(0);
  });

  it("V13-1: a bigger change throws more motes, a reduced tier fewer, and it ends after its span scaled by pace", () => {
    const [small, big, reduced] = [host(), host(), host()];
    createLifeChange(small, { at: HUD, amount: 1, tier: "full", pace: 1 });
    createLifeChange(big, { at: HUD, amount: 8, tier: "full", pace: 1 });
    createLifeChange(reduced, { at: HUD, amount: 8, tier: "reduced", pace: 1 });
    expect(particleCount(big.scene)).toBeGreaterThan(particleCount(small.scene));
    expect(particleCount(reduced.scene)).toBeLessThan(particleCount(big.scene) * 0.6);

    const effect = createLifeChange(host(), { at: HUD, amount: 2, tier: "full", pace: 2 });
    expect(effect.update(1000)).toBe(true);
    expect(effect.update(1000 + LIFE_CHANGE_S * 2000 - 10)).toBe(true);
    expect(effect.update(1000 + LIFE_CHANGE_S * 2000)).toBe(false);
  });
});

describe("counter change", () => {
  function counter(overrides: Partial<CounterChangeParams> = {}): CounterChangeParams {
    return { at: CARD, counterType: "P1P1", change: "added", count: 1, tier: "full", pace: 1, ...overrides };
  }

  function ringScale(scene: Scene) {
    const ring = meshes(scene).find((mesh) => "RING" in ((mesh.material as ShaderMaterial).defines ?? {}));
    return ring?.scale.x ?? Number.NaN;
  }

  it("V13-2: an added counter's ring closes onto the card; a removed one's opens off it", () => {
    const added = host();
    const onto = createCounterChange(added, counter());
    onto.update(1000);
    onto.update(1100);
    const early = ringScale(added.scene);
    onto.update(1300);
    expect(ringScale(added.scene)).toBeLessThan(early);

    const removed = host();
    const off = createCounterChange(removed, counter({ change: "removed" }));
    off.update(1000);
    off.update(1100);
    const start = ringScale(removed.scene);
    off.update(1300);
    expect(ringScale(removed.scene)).toBeGreaterThan(start);
    expect(off.update(1000 + COUNTER_CHANGE_S * 1000)).toBe(false);
  });

  it("V13-2: the tint follows the counter's kind, with a neutral for any other", () => {
    const tintOf = (counterType: string) => {
      const effectHost = host();
      createCounterChange(effectHost, counter({ counterType }));
      const smoke = meshes(effectHost.scene).find((mesh) => "SMOKE" in ((mesh.material as ShaderMaterial).defines ?? {}));
      return (smoke?.material as ShaderMaterial).uniforms.uTint.value.toArray();
    };
    expect(tintOf("P1P1")).toEqual(counterTint("P1P1"));
    expect(tintOf("M1M1")).toEqual(counterTint("M1M1"));
    expect(counterTint("P1P1")).not.toEqual(counterTint("M1M1"));
    expect(counterTint("charge")).toEqual(counterTint("stun"));
  });
});
