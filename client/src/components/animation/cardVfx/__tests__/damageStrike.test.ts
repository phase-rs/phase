import { type InstancedBufferGeometry, Mesh, type Object3D, Scene, Texture } from "three";
import { describe, expect, it, vi } from "vitest";

import { DAMAGE_CAUSE_IMPACT_MS } from "../../../../animation/types.ts";
import type { EffectHost } from "../cardVfxScene.ts";
import {
  createDamageStrike,
  damageStrikeKind,
  type DamageHitParams,
  type DamageStrikeParams,
  HIT_S,
} from "../damageStrike.ts";

function host(): EffectHost {
  return {
    scene: new Scene(),
    backTexture: null,
    placeholderTexture: new Texture(),
    canvasOrigin: () => new DOMRectReadOnly(0, 0, 800, 600),
  };
}

const HUD = { x: 400, y: 40, w: 220, h: 48, angleDeg: 0 };

function hit(overrides: Partial<DamageHitParams> = {}): DamageHitParams {
  return { pose: { x: 300, y: 400, w: 63, h: 88, angleDeg: 90 }, surface: new Texture(), radius: 4, onDone: vi.fn(), ...overrides };
}

function params(overrides: Partial<DamageStrikeParams> = {}): DamageStrikeParams {
  return {
    cause: "fire",
    from: { x: 100, y: 500, w: 63, h: 88, angleDeg: 0 },
    to: HUD,
    hit: null,
    amount: 3,
    tier: "full",
    pace: 1,
    onImpact: vi.fn(),
    ...overrides,
  };
}

const named = (scene: Scene, name: string) => scene.children.find((child) => child.name === name);

/** Every particle instance and mesh a strike draws. */
function strikeLoad(group: Object3D) {
  let particles = 0;
  let meshes = 0;
  group.traverse((object) => {
    if (!(object instanceof Mesh)) return;
    meshes += 1;
    const geometry = object.geometry as InstancedBufferGeometry;
    if (geometry.isInstancedBufferGeometry) particles += geometry.instanceCount;
  });
  return { particles, meshes };
}

describe("damage strike", () => {
  it.each(["fire", "lightning"] as const)(
    "V10-1: a %s strike lands once, at the impact scaled by pace, then ends and removes itself",
    (cause) => {
      const effectHost = host();
      const onImpact = vi.fn();
      const { strike, hit: none } = createDamageStrike(effectHost, params({ cause, pace: 2, onImpact }));
      expect(none).toBeNull();
      expect(named(effectHost.scene, "damage-strike")).toBeDefined();

      const impactMs = DAMAGE_CAUSE_IMPACT_MS * 2;
      expect(strike.update(1000)).toBe(true);
      strike.update(1000 + impactMs - 10);
      expect(onImpact).not.toHaveBeenCalled();
      strike.update(1000 + impactMs);
      strike.update(1000 + impactMs + 50);
      expect(onImpact).toHaveBeenCalledTimes(1);
      expect(strike.update(1000 + impactMs + 4000)).toBe(false);

      strike.dispose(false);
      expect(effectHost.scene.children).toHaveLength(0);
    },
  );

  it("V10-2: the struck card's copy sits on its pose, hidden until the impact, and rocks back to rest", () => {
    const effectHost = host();
    const onDone = vi.fn();
    const { strike, hit: struck } = createDamageStrike(effectHost, params({ to: hit().pose, hit: hit({ onDone }) }));
    const copy = named(effectHost.scene, "damage-hit");
    expect(copy?.position.toArray()).toEqual([300, -400, 0]);
    expect(copy?.rotation.z).toBeCloseTo(-Math.PI / 2);

    struck?.update(1000);
    strike.update(1000);
    struck?.update(1000 + DAMAGE_CAUSE_IMPACT_MS - 10);
    expect(copy?.visible).toBe(false);
    struck?.update(1000 + DAMAGE_CAUSE_IMPACT_MS);
    expect(copy?.visible).toBe(true);
    expect(struck?.update(1000 + DAMAGE_CAUSE_IMPACT_MS + HIT_S * 1000 - 10)).toBe(true);
    expect(struck?.update(1000 + DAMAGE_CAUSE_IMPACT_MS + HIT_S * 1000)).toBe(false);

    struck?.dispose(false);
    expect(onDone).toHaveBeenCalledTimes(1);
    expect(named(effectHost.scene, "damage-hit")).toBeUndefined();
  });

  it("V10-2: a silent dispose of the hit reports nothing", () => {
    const onDone = vi.fn();
    createDamageStrike(host(), params({ hit: hit({ onDone }) })).hit?.dispose(true);
    expect(onDone).not.toHaveBeenCalled();
  });

  it.each(["fire", "lightning"] as const)("V10-3: a reduced %s strike emits fewer particles and casts no board light", (cause) => {
    const full = host();
    const reduced = host();
    createDamageStrike(full, params({ cause, tier: "full" }));
    createDamageStrike(reduced, params({ cause, tier: "reduced" }));
    const [fullLoad, reducedLoad] = [full, reduced].map((h) => strikeLoad(named(h.scene, "damage-strike") as Object3D));

    expect(reducedLoad.particles).toBeLessThan(fullLoad.particles * 0.6);
    expect(reducedLoad.meshes).toBeLessThan(fullLoad.meshes);
  });

  it("V10-4: the warm-up builds every particle, light, bolt and hit program", () => {
    const warm = damageStrikeKind.warmUp(host());
    expect(warm).toHaveLength(9);
    expect(warm.every((object) => object instanceof Mesh)).toBe(true);
    const programs = new Set(
      warm.map((object) => {
        const { vertexShader, fragmentShader, defines } = (object as Mesh).material as unknown as {
          vertexShader: string;
          fragmentShader: string;
          defines: Record<string, string>;
        };
        return JSON.stringify([vertexShader, fragmentShader, defines]);
      }),
    );
    expect(programs.size).toBe(9);
  });
});
