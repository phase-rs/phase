import { type BufferGeometry, type InstancedBufferGeometry, Mesh, NormalBlending, type Object3D, Scene, type ShaderMaterial, Texture } from "three";
import { afterEach, describe, expect, it, vi } from "vitest";

import { DAMAGE_CAUSE_IMPACT_MS } from "../../../../animation/types.ts";
import type { EffectHost } from "../cardVfxScene.ts";
import {
  createDamageBlow,
  createDamageKnockback,
  createDamageStrike,
  type DamageBlowParams,
  damageStrikeKind,
  type DamageHitParams,
  type DamageStrikeParams,
  FIRE_CHARGE_S,
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
  it.each([100, 700])("the fire tail meets the elevated head when travelling from x=%s", (x) => {
    const effectHost = host();
    const { strike } = createDamageStrike(effectHost, params({ from: { ...HUD, x, y: 500 } }));
    const group = named(effectHost.scene, "damage-strike")!;
    const trail = group.children.find((child) => child instanceof Mesh && (child.material as ShaderMaterial).uniforms.uHead) as Mesh<BufferGeometry, ShaderMaterial>;
    const core = group.children.find((child) => child instanceof Mesh && child.renderOrder === 8) as Mesh;
    strike.update(0);
    const halfway = FIRE_CHARGE_S + (DAMAGE_CAUSE_IMPACT_MS / 1000 - FIRE_CHARGE_S) * 0.5 ** (1 / 1.35);
    strike.update(halfway * 1000);
    expect(trail.visible).toBe(true);
    expect(trail.material.uniforms.uHead.value).toBeCloseTo(0.5);
    const positions = trail.geometry.getAttribute("position");
    const along = trail.geometry.getAttribute("aAlong");
    const across = trail.geometry.getAttribute("aAcross");
    const indices = Array.from({ length: along.count }, (_, index) => index);
    const i = indices.find((index) => along.getX(index) === 0.5 && across.getX(index) === 1)!;
    const j = indices.find((index) => along.getX(index) === 0.5 && across.getX(index) === -1)!;
    expect((positions.getX(i) + positions.getX(j)) / 2).toBeCloseTo(core.position.x, 3);
    expect((positions.getY(i) + positions.getY(j)) / 2).toBeCloseTo(core.position.y, 3);
    expect(positions.getZ(i)).toBeCloseTo(core.position.z - 1, 3);
    strike.update(DAMAGE_CAUSE_IMPACT_MS);
    expect(trail.visible).toBe(false);
    strike.dispose(true);
  });

  it.each([10, 600])("a frost bolt over a %s px path fractures completely after impact", (distance) => {
    const effectHost = host();
    const { strike } = createDamageStrike(effectHost, params({
      cause: "water",
      from: { ...HUD, x: HUD.x - distance },
    }));
    const group = named(effectHost.scene, "damage-strike")!;
    const bolt = group.children.find((child) => child instanceof Mesh && (child.material as ShaderMaterial).uniforms.uHead) as Mesh<never, ShaderMaterial>;
    strike.update(0);
    strike.update(DAMAGE_CAUSE_IMPACT_MS);
    expect(bolt.visible).toBe(true);
    strike.update(DAMAGE_CAUSE_IMPACT_MS + 300);
    expect(bolt.material.uniforms.uTail.value).toBe(1);
    expect(bolt.visible).toBe(false);
    strike.dispose(true);
  });

  it.each(["fire", "lightning", "water"] as const)(
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

  it("V13-1: frost draws solid ice and fewer shards at reduced, and marks the struck copy", () => {
    const full = host();
    const reduced = host();
    const { hit: struck } = createDamageStrike(full, params({ cause: "water", tier: "full", hit: hit() }));
    createDamageStrike(reduced, params({ cause: "water", tier: "reduced" }));
    const strike = named(full.scene, "damage-strike") as Object3D;
    const materials: ShaderMaterial[] = [];
    strike.traverse((object) => {
      if (object instanceof Mesh) materials.push(object.material as ShaderMaterial);
    });
    // The ice body and shards are opaque material, with a separate light ring.
    expect(materials.filter((material) => material.defines.ICE !== undefined || material.uniforms.uHead).every((material) => material.blending === NormalBlending)).toBe(true);
    const [fullLoad, reducedLoad] = [full, reduced].map((h) => strikeLoad(named(h.scene, "damage-strike") as Object3D));
    expect(reducedLoad.particles).toBeLessThan(fullLoad.particles * 0.6);

    struck?.dispose(true);
    const soaked = createDamageStrike(host(), params({ cause: "water", hit: hit() })).hit as unknown as { materials: { card: ShaderMaterial } };
    const scorched = createDamageStrike(host(), params({ cause: "fire", hit: hit() })).hit as unknown as { materials: { card: ShaderMaterial } };
    expect(soaked.materials.card.defines).toEqual({ FROST: "" });
    expect(scorched.materials.card.defines).toEqual({});
  });

  it("V10-4: the warm-up builds every particle, light, bolt, jet and hit program", () => {
    const warm = damageStrikeKind.warmUp(host());
    expect(warm).toHaveLength(16);
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
    expect(programs.size).toBe(16);
    for (const cause of ["fire", "lightning", "water"] as const) {
      const effectHost = host();
      const { strike } = createDamageStrike(effectHost, params({ cause }));
      effectHost.scene.traverse((object) => {
        if (!(object instanceof Mesh)) return;
        const { vertexShader, fragmentShader, defines } = object.material as ShaderMaterial;
        expect(programs.has(JSON.stringify([vertexShader, fragmentShader, defines]))).toBe(true);
      });
      strike.dispose(true);
    }
  });
});

describe("damage blow", () => {
  const FROM = { x: 100, y: 400, w: 63, h: 88, angleDeg: 0 };
  const TO = { x: 400, y: 400, w: 63, h: 88, angleDeg: 0 };

  afterEach(() => {
    vi.restoreAllMocks();
  });

  function blow(overrides: Partial<DamageBlowParams> = {}): DamageBlowParams {
    return { from: FROM, to: TO, amount: 3, tier: "full", pace: 1, startMs: 1000, impactS: 0.3, ...overrides };
  }

  /** Replaces `Math.random` with a fixed sequence (mulberry32), so a sampled
   *  spread is the same on every run. */
  function seedRandom(seed: number) {
    let a = seed;
    vi.spyOn(Math, "random").mockImplementation(() => {
      a = (a + 0x6d2b79f5) | 0;
      let t = Math.imul(a ^ (a >>> 15), 1 | a);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    });
  }

  function meshes(group: Object3D) {
    const found: Mesh[] = [];
    group.traverse((object) => {
      if (object instanceof Mesh) found.push(object);
    });
    return found;
  }

  /** The mean of every dust and grit velocity's component along +x (world). */
  function meanPushX(group: Object3D) {
    let sum = 0;
    let n = 0;
    for (const mesh of meshes(group)) {
      const velocity = (mesh.geometry as InstancedBufferGeometry).getAttribute("aVel");
      if (!velocity) continue;
      for (let i = 0; i < velocity.count; i += 1) {
        sum += Math.sign(velocity.getX(i));
        n += 1;
      }
    }
    return sum / n;
  }

  it("V12-1: a blow shows its ring only from the impact, scaled by pace, and ends after its dust settles", () => {
    const effectHost = host();
    const effect = createDamageBlow(effectHost, blow({ pace: 2 }));
    const group = named(effectHost.scene, "damage-blow") as Object3D;
    const ring = meshes(group).find((mesh) => "RING" in ((mesh.material as ShaderMaterial).defines ?? {}));

    expect(effect.update(1000)).toBe(true);
    effect.update(1000 + 0.3 * 2 * 1000 - 10);
    expect(ring?.visible).toBe(false);
    effect.update(1000 + 0.3 * 2 * 1000 + 40);
    expect(ring?.visible).toBe(true);
    expect(effect.update(1000 + 4000)).toBe(false);

    effect.dispose(false);
    expect(effectHost.scene.children).toHaveLength(0);
  });

  it("V12-1: a blow runs on the slam's clock, so a late first frame still lands at the slam's impact", () => {
    const effectHost = host();
    const effect = createDamageBlow(effectHost, blow());
    const group = named(effectHost.scene, "damage-blow") as Object3D;
    const ring = meshes(group).find((mesh) => "RING" in ((mesh.material as ShaderMaterial).defines ?? {}));

    // The first frame arrives after the slam has already struck.
    effect.update(1000 + 0.3 * 1000 + 40);
    expect(ring?.visible).toBe(true);
  });

  it("V12-1: a blow gives off no light: dust and grit only, and one pale ring", () => {
    const effectHost = host();
    createDamageBlow(effectHost, blow());
    const programs = meshes(named(effectHost.scene, "damage-blow") as Object3D).map(
      (mesh) => Object.keys((mesh.material as ShaderMaterial).defines ?? {}),
    );
    expect(programs.sort()).toEqual([["RING"], ["SMOKE"], ["SMOKE"]]);
  });

  it("V12-2: dust is thrown on along the blow, and all round with no one direction", () => {
    seedRandom(12);
    const along = host();
    createDamageBlow(along, blow());
    expect(meanPushX(named(along.scene, "damage-blow") as Object3D)).toBeGreaterThan(0.3);

    const around = host();
    createDamageBlow(around, blow({ from: null }));
    expect(Math.abs(meanPushX(named(around.scene, "damage-blow") as Object3D))).toBeLessThan(0.3);
  });

  it("V12-2: a reduced blow throws fewer motes", () => {
    const [full, reduced] = [host(), host()];
    createDamageBlow(full, blow());
    createDamageBlow(reduced, blow({ tier: "reduced" }));
    const [fullLoad, reducedLoad] = [full, reduced].map((h) => strikeLoad(named(h.scene, "damage-blow") as Object3D));
    expect(reducedLoad.particles).toBeLessThan(fullLoad.particles * 0.6);
  });
});

describe("damage knockback", () => {
  const STRUCK = { x: 400, y: 300, w: 63, h: 88, angleDeg: 0 };
  const uniforms = (scene: Scene) => {
    const group = named(scene, "damage-hit") as Object3D;
    return ((group.children[0] as Mesh).material as ShaderMaterial).uniforms;
  };

  it("V16-6: on the slam's clock, the copy shows only from the impact, rocks back unmarked, and reports its impact once", () => {
    const effectHost = host();
    const onImpact = vi.fn();
    const copy = createDamageKnockback(effectHost, {
      hit: hit({ pose: STRUCK }),
      from: { x: 100, y: 300, w: 63, h: 88, angleDeg: 0 },
      occluder: null,
      amount: 3,
      pace: 2,
      startMs: 1000,
      impactS: 0.2,
      onImpact,
    });
    const group = named(effectHost.scene, "damage-hit") as Object3D;
    // A first frame late on the slam's clock still lands at the slam's impact.
    copy.update(1000 + 400 - 10);
    expect(group.visible).toBe(false);
    expect(onImpact).not.toHaveBeenCalled();
    copy.update(1000 + 400 + 60);
    copy.update(1000 + 400 + 90);
    expect(group.visible).toBe(true);
    expect(onImpact).toHaveBeenCalledTimes(1);
    const U = uniforms(effectHost.scene);
    // Pushed on along the blow, from the left: rightward.
    expect(U.uPush.value.x).toBeGreaterThan(0);
    expect(U.uScorch.value).toBe(0);
    expect(copy.update(1000 + 400 + HIT_S * 2000 + 10)).toBe(false);
  });

  it("V16-6: the card lying over it is cut out of the copy, wherever it is each frame", () => {
    const effectHost = host();
    let over: { pose: typeof STRUCK; radius: number } | null = { pose: { ...STRUCK, x: 390, angleDeg: 10 }, radius: 5 };
    const copy = createDamageKnockback(effectHost, {
      hit: hit({ pose: STRUCK }),
      from: null,
      occluder: () => over,
      amount: 3,
      pace: 1,
      startMs: 1000,
      impactS: 0.2,
      onImpact: vi.fn(),
    });
    copy.update(1250);
    const U = uniforms(effectHost.scene);
    expect(U.uOccCenter.value.toArray()).toEqual([390, -300]);
    expect(U.uOccHalf.value.toArray()).toEqual([31.5, 44]);
    expect(U.uOccAngle.value).toBeCloseTo((10 * Math.PI) / 180);
    expect(U.uOccRadius.value).toBe(5);
    over = null;
    copy.update(1300);
    expect(U.uOccHalf.value.toArray()).toEqual([0, 0]);
  });
});
