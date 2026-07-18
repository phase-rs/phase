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

  it("removes a zero-duration effect instead of leaking it forever", () => {
    // duration 0 makes elapsed/duration NaN at elapsed 0; NaN >= 1 is false, so
    // the effect would never complete — it leaks and keeps the rAF loop alive.
    const system = new ParticleSystem();
    let completed = false;
    const effect: ActiveEffect = {
      startTime: 0,
      duration: 0,
      update() {},
      onComplete() {
        completed = true;
      },
    };
    system.addEffect(effect);
    system["updateEffects"](0);

    expect(system["effects"].length).toBe(0);
    expect(completed).toBe(true);
  });
});
