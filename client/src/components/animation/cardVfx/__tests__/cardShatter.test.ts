import { type BufferGeometry, Mesh, Points, Scene, Texture } from "three";
import { describe, expect, it, vi } from "vitest";

import {
  buildShards,
  cardShatterKind,
  createCardShatter,
  type CardShatterParams,
  SHATTER_CRACK_S,
  SHATTER_FALL_S,
  SHATTER_SHARDS,
} from "../cardShatter.ts";
import type { EffectHost } from "../cardVfxScene.ts";

function host(): EffectHost {
  return {
    scene: new Scene(),
    backTexture: null,
    placeholderTexture: new Texture(),
    canvasOrigin: () => new DOMRectReadOnly(0, 0, 800, 600),
  };
}

function params(overrides: Partial<CardShatterParams> = {}): CardShatterParams {
  return {
    pose: { x: 300, y: 400, w: 63, h: 88, angleDeg: 90 },
    surface: new Texture(),
    radius: 4,
    impact: { u: 0.5, v: 0.5 },
    tier: "full",
    pace: 1,
    pixelRatio: 1,
    startMs: null,
    onDone: vi.fn(),
    ...overrides,
  };
}

/** The summed area of every shard's top face, in card px². */
function topArea(geometry: BufferGeometry): number {
  const card = geometry.getAttribute("aCard");
  const face = geometry.getAttribute("aFace");
  let area = 0;
  for (let i = 0; i < card.count; i += 3) {
    if (face.getX(i) !== 0) continue;
    const [ax, ay, bx, by, cx, cy] = [0, 1, 2].flatMap((k) => [card.getX(i + k), card.getY(i + k)]);
    area += Math.abs((bx - ax) * (cy - ay) - (cx - ax) * (by - ay)) / 2;
  }
  return area;
}

describe("card shatter", () => {
  it.each([
    [0.5, 0.5],
    [0.1, 0.9],
  ])("V8-4: the shards tile the whole card, broken at (%s, %s)", (u, v) => {
    const { geometry, edges } = buildShards(63, 88, 63 * u, 88 * v, SHATTER_SHARDS.full);

    expect(topArea(geometry)).toBeCloseTo(63 * 88, 0);
    expect(edges.length).toBeGreaterThan(0);
    // Every crack arrival lies in 0–1; an edge that cracks only at the break is 2.
    const arrive = geometry.getAttribute("aArrive");
    for (let i = 0; i < arrive.count; i++) expect([0, 2].includes(arrive.getX(i)) || arrive.getX(i) <= 1).toBe(true);
  });

  it("V8-5: the shatter sits on the card's pose, runs its crack and fall scaled by pace, then removes itself", () => {
    const effectHost = host();
    const onDone = vi.fn();
    const shatter = createCardShatter(effectHost, params({ pace: 2, onDone }));
    const [group] = effectHost.scene.children;
    expect(group.name).toBe("card-shatter");
    expect(group.position.toArray()).toEqual([300, -400, 0]);
    expect(group.rotation.z).toBeCloseTo(-Math.PI / 2);

    const totalMs = (SHATTER_CRACK_S + SHATTER_FALL_S) * 1000 * 2;
    expect(shatter.update(1000)).toBe(true);
    expect(shatter.update(1000 + totalMs)).toBe(true);
    expect(shatter.update(1000 + totalMs + 200)).toBe(false);

    shatter.dispose(false);
    expect(effectHost.scene.children).toHaveLength(0);
    expect(onDone).toHaveBeenCalledTimes(1);
  });

  it("V8-5: a silent dispose reports nothing", () => {
    const onDone = vi.fn();
    createCardShatter(host(), params({ onDone })).dispose(true);
    expect(onDone).not.toHaveBeenCalled();
  });

  it.each([
    ["full", ["card-shatter-shards", "card-shatter-shadow", "card-shatter-flakes"]],
    ["reduced", ["card-shatter-shards"]],
  ] as const)("V8-6: at tier %s the shards carry shadows and flakes only when full", (tier, names) => {
    const effectHost = host();
    createCardShatter(effectHost, params({ tier }));
    const [group] = effectHost.scene.children;
    expect(group.children.map((child) => child.name)).toEqual(names);
  });

  it("V8-7: the warm-up declares the shard, shadow and flake passes", () => {
    const warm = cardShatterKind.warmUp(host());
    expect(warm.map((object) => object.name)).toEqual([
      "card-shatter-warmup",
      "card-shatter-shadow-warmup",
      "card-shatter-flakes-warmup",
    ]);
    expect(warm[0]).toBeInstanceOf(Mesh);
    expect(warm[2]).toBeInstanceOf(Points);
  });
});
