import { InstancedBufferGeometry, Mesh, Scene, type ShaderMaterial, Texture } from "three";
import { describe, expect, it, vi } from "vitest";

import { BURN_S, BURN_TAIL_S, cardBurnKind, createCardBurn, type CardBurnParams } from "../cardBurn.ts";
import type { EffectHost } from "../cardVfxScene.ts";

function host(): EffectHost {
  return { scene: new Scene(), backTexture: null, placeholderTexture: new Texture(), canvasOrigin: () => new DOMRectReadOnly(0, 0, 800, 600) };
}

function params(overrides: Partial<CardBurnParams> = {}): CardBurnParams {
  return {
    pose: { x: 300, y: 400, w: 63, h: 88, angleDeg: 90 }, surface: new Texture(), radius: 4,
    impact: { u: 0.4, v: 0.3 }, amount: 3, tier: "full", pace: 1, startMs: null, onDone: vi.fn(), ...overrides,
  };
}

describe("card burn", () => {
  it.each([0.5, 1, 2])("finishes at pace %s and frees its shared surface and geometry once", (pace) => {
    const effectHost = host();
    const p = params({ pace });
    const freeSurface = vi.spyOn(p.surface, "dispose");
    const effect = createCardBurn(effectHost, p);
    const group = effectHost.scene.children.find((object) => object.name === "card-burn")!;
    const card = group.children[0] as Mesh;
    const freeGeometry = vi.spyOn(card.geometry, "dispose");
    expect(group.position.toArray()).toEqual([300, -400, 0]);
    expect(group.rotation.z).toBeCloseTo(-Math.PI / 2);
    expect(effect.update(1000)).toBe(true);
    expect(effect.update(1000 + (BURN_S + BURN_TAIL_S) * 1000 * pace - 1)).toBe(true);
    expect(effect.update(1000 + (BURN_S + BURN_TAIL_S) * 1000 * pace)).toBe(false);
    effect.dispose(false);
    expect(effectHost.scene.children).toHaveLength(0);
    expect(freeSurface).toHaveBeenCalledTimes(1);
    expect(freeGeometry).toHaveBeenCalledTimes(1);
    expect(p.onDone).toHaveBeenCalledTimes(1);
  });

  it("silently replacing a burn does not release the next effect's veil", () => {
    const p = params();
    const effect = createCardBurn(host(), p);
    effect.dispose(true);
    expect(p.onDone).not.toHaveBeenCalled();
  });

  it("the card, flames and particle arrival share a key, and reduced tier halves the particle load", () => {
    const loads = [];
    for (const tier of ["full", "reduced"] as const) {
      const effectHost = host();
      const effect = createCardBurn(effectHost, params({ tier }));
      const group = effectHost.scene.children.find((object) => object.name === "card-burn")!;
      const card = group.children[0] as Mesh;
      const uniforms = (card.material as ShaderMaterial).uniforms;
      const fire = effectHost.scene.children.find((object) => object.name === "card-burn-flame") as Mesh;
      expect((fire.material as ShaderMaterial).uniforms.uKeyImpact).toBe(uniforms.uKeyImpact);
      let particles = 0;
      group.traverse((object) => {
        if (!(object instanceof Mesh) || !(object.geometry instanceof InstancedBufferGeometry)) return;
        const material = object.material as ShaderMaterial;
        expect(material.defines).toHaveProperty("FROM_KEY");
        expect(material.uniforms.uKeyImpact).toBe(uniforms.uKeyImpact);
        particles += object.geometry.instanceCount;
      });
      loads.push(particles);
      effect.update(1000);
      expect(uniforms.uFront.value).toBe(-0.08);
      effect.update(1000 + BURN_S * 500);
      expect(uniforms.uFront.value).toBeCloseTo(-0.08 + 1.55 / 2);
      effect.dispose(true);
    }
    expect(loads).toEqual([135, 68]);
    expect(cardBurnKind.warmUp(host())).toHaveLength(2);
  });
});
