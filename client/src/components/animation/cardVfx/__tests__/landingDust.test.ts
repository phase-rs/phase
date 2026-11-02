import { Color, Scene } from "three";
import { describe, expect, it } from "vitest";

import type { EffectHost } from "../cardVfxScene.ts";
import { createLandingDust, dustColor, landingDustKind } from "../landingDust.ts";

function host(): EffectHost {
  return {
    scene: new Scene(),
    backTexture: null,
    placeholderTexture: null as never,
    canvasOrigin: () => new DOMRectReadOnly(0, 0, 800, 600),
  };
}

const POSE = { x: 300, y: 400, w: 63, h: 88, angleDeg: 0 };

describe("landing dust", () => {
  it("V4-4: plain dust for no colour, tinted toward the card's colours otherwise", () => {
    const plain = dustColor(null);
    expect(dustColor([]).equals(plain)).toBe(true);

    const red = dustColor(["Red"]);
    const blue = dustColor(["Blue"]);
    expect(red.equals(plain)).toBe(false);
    expect(red.r).toBeGreaterThan(blue.r);
    expect(blue.b).toBeGreaterThan(red.b);
    // Two colours sit between the two single-colour tints.
    const both = dustColor(["Red", "Blue"]);
    expect(both.r).toBeLessThan(red.r);
    expect(both.r).toBeGreaterThan(blue.r);
    expect(red).toBeInstanceOf(Color);
  });

  it("V4-5: the dust runs for its life scaled by pace, then removes itself from the scene", () => {
    const effectHost = host();
    const dust = createLandingDust(effectHost, POSE, ["Green"], 2, 1);
    expect(effectHost.scene.children.map((child) => child.name)).toEqual(["landing-dust"]);

    expect(dust.update(1000)).toBe(true);
    // 0.7 s of life at pace 2 is 1.4 s.
    expect(dust.update(2300)).toBe(true);
    expect(dust.update(2500)).toBe(false);

    dust.dispose(false);
    expect(effectHost.scene.children).toHaveLength(0);
  });

  it("V4-6: the warm-up declares one dust mesh, built as the effect builds it", () => {
    const [warm] = landingDustKind.warmUp(host());
    expect(warm.name).toBe("landing-dust-warmup");
    expect(warm.type).toBe("Points");
  });
});
