import { Mesh, type Object3D, Scene, type ShaderMaterial, Texture } from "three";
import { describe, expect, it } from "vitest";

import { boardSweepKind, createBoardSweep } from "../boardSweep.ts";
import type { SweepLook } from "../cardFlightSpecs.ts";
import type { EffectHost } from "../cardVfxScene.ts";
import { SWEEP_TRAVEL_S, type SweepPath } from "../sweepPath.ts";

function host(): EffectHost {
  return {
    scene: new Scene(),
    backTexture: null,
    placeholderTexture: new Texture(),
    canvasOrigin: () => new DOMRectReadOnly(0, 0, 800, 600),
  };
}

const PATH: SweepPath = {
  from: [400, 650],
  to: [400, 100],
  halfWidth: 300,
  hits: [
    { objectId: 1, at: [300, 450], atS: 0.2 },
    { objectId: 2, at: [300, 250], atS: 0.45 },
  ],
};

function meshes(effectHost: EffectHost) {
  const group = effectHost.scene.getObjectByName("board-sweep") as Object3D;
  const found: Mesh<never, ShaderMaterial>[] = [];
  group.traverse((object) => {
    if (object instanceof Mesh) found.push(object as Mesh<never, ShaderMaterial>);
  });
  return found;
}

const sweepOf = (effectHost: EffectHost, look: SweepLook, pace = 1) =>
  createBoardSweep(effectHost, { path: PATH, look, tier: "full", pace, startMs: 1000 });

describe("board sweep", () => {
  it("V15-6: the wave throws water over the cards it reaches, the smoke curls up from them, and the light is light alone", () => {
    const defines = (look: SweepLook) => {
      const effectHost = host();
      sweepOf(effectHost, look);
      return meshes(effectHost).map((mesh) => Object.keys(mesh.material.defines ?? {}).join());
    };
    expect(defines("tsunami")).toEqual(["WAVE", "SMOKE", "DROP"]);
    expect(defines("smoke")).toEqual(["SMOKE", "SMOKE"]);
    expect(defines("light")).toEqual(["LIGHT"]);
  });

  it("V15-6: the front crosses the board on the paced clock, then fades and ends", () => {
    const effectHost = host();
    const sweep = sweepOf(effectHost, "light", 2);
    const front = () => meshes(effectHost)[0].material.uniforms;
    sweep.update(1000);
    const setOut = front().uFront.value as number;
    sweep.update(1000 + SWEEP_TRAVEL_S * 1000);
    expect(front().uFront.value).toBeGreaterThan(setOut);
    expect(front().uGain.value).toBeGreaterThan(0.9);
    sweep.update(1000 + SWEEP_TRAVEL_S * 2000);
    const crossed = front().uFront.value as number;
    expect(crossed - setOut).toBeCloseTo(PATH.from[1] - PATH.to[1]);
    expect(sweep.update(1000 + 60_000)).toBe(false);
    sweep.dispose(false);
    expect(effectHost.scene.children).toHaveLength(0);
  });

  it("V15-6: the warm-up builds every look's front and the wave's and smoke's particles", () => {
    const warm = boardSweepKind.warmUp(host());
    const defines = warm.map((object) => Object.keys(((object as Mesh).material as ShaderMaterial).defines ?? {}));
    expect(defines).toEqual([["WAVE"], ["LIGHT"], ["SMOKE"], ["SMOKE"], ["DROP"]]);
  });
});
