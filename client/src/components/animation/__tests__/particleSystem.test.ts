import { describe, expect, it } from "vitest";

import { ParticleSystem, type ActiveEffect } from "../particleSystem";

describe("ParticleSystem", () => {
  it("does not update delayed effects before their start time", () => {
    const system = new ParticleSystem();
    let updates = 0;
    const effect: ActiveEffect = {
      startTime: 1000,
      duration: 100,
      update() {
        updates++;
      },
    };

    system.addEffect(effect);
    system["updateEffects"](999);

    expect(updates).toBe(0);
  });
});
