import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Material, Mesh, Object3D, Texture } from "three";

// The real component, with WebGL and image loading stubbed: every
// `renderer.render` records what the canvas would show, and every texture load
// waits until the test completes it.
const harness = vi.hoisted(() => ({
  renderers: [] as Array<{
    domElement: HTMLCanvasElement;
    disposed: boolean;
    draws: Array<{ frontMap: unknown; frontColor: number; glowVisible: boolean; glowRed: number; tileY: number }>;
    drawsAfterDispose: number;
  }>,
  loads: [] as Array<{ url: string; onLoad: (texture: Texture) => void }>,
}));

vi.mock("three", async (importOriginal) => {
  const actual = await importOriginal<typeof import("three")>();

  class RecordingRenderer {
    domElement = document.createElement("canvas");
    disposed = false;
    draws: (typeof harness.renderers)[number]["draws"] = [];
    drawsAfterDispose = 0;
    constructor() {
      harness.renderers.push(this);
    }
    setSize() {}
    setPixelRatio() {}
    render(scene: Object3D) {
      if (this.disposed) {
        this.drawsAfterDispose += 1;
        return;
      }
      let tile: Mesh | undefined;
      let glow: Mesh | undefined;
      scene.traverse((node) => {
        const mesh = node as Mesh;
        if (!mesh.isMesh) return;
        if (Array.isArray(mesh.material)) tile = mesh;
        else if ((mesh.material as Material).blending === actual.AdditiveBlending) glow = mesh;
      });
      if (!tile || !glow) throw new Error("scene is missing the tile or the glow");
      const front = (tile.material as InstanceType<typeof actual.MeshStandardMaterial>[])[1];
      const glowMaterial = glow.material as InstanceType<typeof actual.MeshBasicMaterial>;
      this.draws.push({
        frontMap: front.map,
        frontColor: front.color.getHex(),
        glowVisible: glow.visible,
        glowRed: glowMaterial.color.r,
        tileY: tile.position.y,
      });
    }
    dispose() {
      this.disposed = true;
    }
  }

  class DeferredTextureLoader {
    setCrossOrigin() {
      return this;
    }
    load(url: string, onLoad: (texture: Texture) => void) {
      harness.loads.push({ url, onLoad });
      return new actual.Texture();
    }
  }

  return { ...actual, WebGLRenderer: RecordingRenderer, TextureLoader: DeferredTextureLoader };
});

import { Texture as RealTexture } from "three";
import { DexterityFlip3D } from "../DexterityFlip3D.tsx";
import { FLIGHT_MS, FLIGHT_TOTAL_MS, RELEASE_HEIGHT, type DexterityFlipResult } from "../dexterityFlip.ts";

const KEPT = "52,211,153";
const SACRIFICED = "251,113,133";
const ART = "https://example.test/art.jpg";
const OTHER_ART = "https://example.test/other.jpg";
const FACE_UP: DexterityFlipResult = { landing: "faceUp", fullTurns: 2 };
const FACE_DOWN: DexterityFlipResult = { landing: "faceDown", fullTurns: 1 };
const REST_MS = FLIGHT_MS.hold + FLIGHT_MS.fall + FLIGHT_MS.bounce;
const MID_FALL_MS = FLIGHT_MS.hold + FLIGHT_MS.fall / 2;

let frames = new Map<number, FrameRequestCallback>();
let nextFrameId = 0;
const cancelled: number[] = [];

/** A permissive 2D context: every method is a no-op returning another stub, so
 *  the canvas textures draw under happy-dom. */
const stubContext: unknown = new Proxy(function () {}, {
  get: () => stubContext,
  apply: () => stubContext,
});

/** Runs every pending animation frame at `elapsedMs` after mount. */
function runFrames(elapsedMs: number) {
  act(() => {
    const pending = [...frames.values()];
    frames = new Map();
    for (const frame of pending) frame(elapsedMs);
  });
}

/** Completes the oldest pending texture load with a fresh texture. */
function completeLoad(): Texture {
  const load = harness.loads.shift();
  if (!load) throw new Error("no texture load is pending");
  const texture = new RealTexture();
  texture.image = { width: 626, height: 457 };
  vi.spyOn(texture, "dispose");
  act(() => load.onLoad(texture));
  return texture;
}

const onlyRenderer = () => {
  expect(harness.renderers).toHaveLength(1);
  return harness.renderers[0];
};
const lastDraw = () => {
  const { draws } = onlyRenderer();
  return draws[draws.length - 1];
};

beforeEach(() => {
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(stubContext as never);
  // The flight starts at `performance.now()`; pin it so frame timestamps are
  // elapsed time.
  vi.spyOn(performance, "now").mockReturnValue(0);
  vi.stubGlobal("requestAnimationFrame", (frame: FrameRequestCallback) => {
    nextFrameId += 1;
    frames.set(nextFrameId, frame);
    return nextFrameId;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => {
    cancelled.push(id);
    frames.delete(id);
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  frames = new Map();
  nextFrameId = 0;
  cancelled.length = 0;
  harness.renderers.length = 0;
  harness.loads.length = 0;
});

describe("DexterityFlip3D settle notification", () => {
  it("fires once per flight, when the tile comes to rest", () => {
    const onSettle = vi.fn();
    const { rerender } = render(<DexterityFlip3D result={FACE_UP} onSettle={onSettle} />);

    runFrames(REST_MS - 1);
    expect(onSettle).not.toHaveBeenCalled();
    runFrames(REST_MS);
    expect(onSettle).toHaveBeenCalledTimes(1);
    // The reveal keeps drawing after rest without notifying again, then stops.
    runFrames(FLIGHT_TOTAL_MS);
    expect(onSettle).toHaveBeenCalledTimes(1);
    expect(frames.size).toBe(0);

    // A new result is a new flight with its own notification.
    rerender(<DexterityFlip3D result={FACE_DOWN} onSettle={onSettle} />);
    runFrames(FLIGHT_TOTAL_MS);
    expect(onSettle).toHaveBeenCalledTimes(2);
  });
});

describe("DexterityFlip3D appearance changes", () => {
  it("restyles mid-flight without restarting the flight", () => {
    const onSettle = vi.fn();
    const { rerender } = render(
      <DexterityFlip3D result={FACE_UP} artSrc={ART} accentRgb={KEPT} onSettle={onSettle} />,
    );
    runFrames(MID_FALL_MS);
    const renderer = onlyRenderer();
    expect(lastDraw().tileY).toBeLessThan(RELEASE_HEIGHT);

    // A flight restarted now would start its clock here and be back in the
    // hold, at the release height, for the next frame.
    vi.mocked(performance.now).mockReturnValue(MID_FALL_MS);
    rerender(<DexterityFlip3D result={FACE_UP} artSrc={OTHER_ART} accentRgb={SACRIFICED} onSettle={onSettle} />);
    const art = completeLoad(); // the first load was superseded
    expect(art.dispose).toHaveBeenCalled();
    const otherArt = completeLoad();

    // Same renderer, same pending frame: the flight carries on from mid-fall.
    expect(onlyRenderer()).toBe(renderer);
    expect(renderer.domElement.isConnected).toBe(true);
    expect(frames.size).toBe(1);
    // Mid-flight the loop's next frame shows the change; nothing draws early.
    expect(renderer.draws).toHaveLength(1);
    runFrames(MID_FALL_MS + 100);
    expect(lastDraw().tileY).toBeLessThan(RELEASE_HEIGHT);
    expect(lastDraw().frontMap).toBe(otherArt);
    expect(lastDraw().glowRed).toBeCloseTo(251 / 255);

    runFrames(FLIGHT_TOTAL_MS);
    expect(onSettle).toHaveBeenCalledTimes(1);
  });

  it("redraws a finished flight once per art load, accent change and art removal", () => {
    const { rerender } = render(<DexterityFlip3D result={FACE_UP} artSrc={ART} accentRgb={KEPT} />);
    runFrames(FLIGHT_TOTAL_MS);
    const renderer = onlyRenderer();
    expect(frames.size).toBe(0);
    let draws = renderer.draws.length;
    expect(lastDraw().frontMap).toBeNull();

    // Art that arrives after landing reaches the canvas.
    const art = completeLoad();
    expect(renderer.draws).toHaveLength(++draws);
    expect(lastDraw().frontMap).toBe(art);
    expect(lastDraw().frontColor).toBe(0xffffff);

    // So does a new accent.
    rerender(<DexterityFlip3D result={FACE_UP} artSrc={ART} accentRgb={SACRIFICED} />);
    expect(renderer.draws).toHaveLength(++draws);
    expect(lastDraw().glowRed).toBeCloseTo(251 / 255);

    // And removing the accent hides the glow.
    rerender(<DexterityFlip3D result={FACE_UP} artSrc={ART} />);
    expect(renderer.draws).toHaveLength(++draws);
    expect(lastDraw().glowVisible).toBe(false);

    // And removing the art puts the placeholder back.
    rerender(<DexterityFlip3D result={FACE_UP} artSrc={null} />);
    expect(renderer.draws).toHaveLength(++draws);
    expect(lastDraw().frontMap).toBeNull();
    expect(art.dispose).toHaveBeenCalled();

    // None of it restarted the flight.
    expect(onlyRenderer()).toBe(renderer);
    expect(frames.size).toBe(0);
  });
});

describe("DexterityFlip3D unmount", () => {
  it("cancels the pending frame, disposes the renderer and discards a late texture", () => {
    const onSettle = vi.fn();
    const { unmount } = render(<DexterityFlip3D result={FACE_UP} artSrc={ART} onSettle={onSettle} />);
    runFrames(MID_FALL_MS);
    const renderer = onlyRenderer();
    const [pending] = [...frames.keys()];

    unmount();

    expect(cancelled).toContain(pending);
    expect(frames.size).toBe(0);
    expect(renderer.disposed).toBe(true);
    expect(renderer.domElement.parentNode).toBeNull();

    const late = completeLoad();
    expect(late.dispose).toHaveBeenCalled();
    expect(renderer.drawsAfterDispose).toBe(0);
    expect(onSettle).not.toHaveBeenCalled();
  });

  it("disposes loaded art without drawing after a finished flight unmounts", () => {
    const { unmount } = render(<DexterityFlip3D result={FACE_UP} artSrc={ART} />);
    runFrames(FLIGHT_TOTAL_MS);
    const art = completeLoad();
    const renderer = onlyRenderer();

    unmount();

    expect(renderer.disposed).toBe(true);
    expect(art.dispose).toHaveBeenCalled();
    expect(renderer.drawsAfterDispose).toBe(0);
  });
});
