import { describe, expect, it } from "vitest";

import {
  DAMAGE_FLURRY_PROJECTILE_MAX,
  DAMAGE_FLURRY_TRAIL_PARTICLE_MAX,
} from "../../../animation/types";
import type { ActiveEffect, ParticleSystem } from "../particleSystem";
import { damageFlurryProjectileCount, emitDamageFlurry } from "../particleEffects";

describe("particleEffects", () => {
  it("caps damage flurry projectile count for huge combat batches", () => {
    expect(damageFlurryProjectileCount(700)).toBe(DAMAGE_FLURRY_PROJECTILE_MAX);
  });

  it("uses the supplied impact duration and caps trail particles per flurry", () => {
    const effects: ActiveEffect[] = [];
    let emittedParticles = 0;
    const system = {
      emit(partials: unknown[]) {
        emittedParticles += partials.length;
      },
      addEffect(effect: ActiveEffect) {
        effects.push(effect);
      },
    } as unknown as ParticleSystem;
    const impactDelay = 520;

    emitDamageFlurry(system, [{ x: 0, y: 0 }], { x: 100, y: 100 }, 700, 700, impactDelay);

    const projectileEffects = effects.slice(0, DAMAGE_FLURRY_PROJECTILE_MAX);
    const impactEffect = effects[DAMAGE_FLURRY_PROJECTILE_MAX];
    const lastProjectile = projectileEffects[projectileEffects.length - 1];
    expect(projectileEffects).toHaveLength(DAMAGE_FLURRY_PROJECTILE_MAX);
    expect(projectileEffects[0].duration).toBe(impactDelay);
    expect(lastProjectile.startTime + lastProjectile.duration).toBe(impactEffect.startTime);
    expect(impactEffect.startTime).toBe(projectileEffects[0].startTime + impactDelay);

    for (const effect of projectileEffects) effect.update(0, system);
    for (const effect of projectileEffects) effect.update(0.2, system);

    expect(emittedParticles).toBe(DAMAGE_FLURRY_TRAIL_PARTICLE_MAX);
  });

  it("caps stagger span so short flurries still finish at impact", () => {
    const effects: ActiveEffect[] = [];
    const system = {
      emit() {},
      addEffect(effect: ActiveEffect) {
        effects.push(effect);
      },
    } as unknown as ParticleSystem;
    const impactDelay = 40;

    emitDamageFlurry(system, [{ x: 0, y: 0 }], { x: 100, y: 100 }, 700, 700, impactDelay);

    const projectileEffects = effects.slice(0, DAMAGE_FLURRY_PROJECTILE_MAX);
    const impactEffect = effects[DAMAGE_FLURRY_PROJECTILE_MAX];
    const lastProjectile = projectileEffects[projectileEffects.length - 1];

    expect(lastProjectile.startTime + lastProjectile.duration).toBe(impactEffect.startTime);
    expect(impactEffect.startTime).toBe(projectileEffects[0].startTime + impactDelay);
  });
});
