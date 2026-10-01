import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { createRef, StrictMode } from "react";
import type { Mesh, Object3D, Points, Scene, ShaderMaterial, Texture } from "three";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useCardBackImage, useCardImage } from "../../../../hooks/useCardImage.ts";
import { CARD_BACK_URL } from "../../../../services/scryfall.ts";
import { assetKey, catalogRoot, packId } from "../../../../services/visualPacks/types.ts";
import { DAMAGE_CAUSE_IMPACT_MS } from "../../../../animation/types.ts";
import { useAnimationStore } from "../../../../stores/animationStore.ts";
import { useGameStore } from "../../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../../stores/preferencesStore.ts";
import type { AnimationImageSnapshot } from "../../ResolvedAnimationImage.tsx";
import {
  ABANDON_FADE_MS,
  CAST_FLIGHT_MS,
  DRAW_FLIGHT_MS,
  LAND_REVEAL_WAIT_MAX_MS,
  LAND_STATIONARY_WAIT_MAX_MS,
  RESOLVE_FLIGHT_MS,
  SETTLE_MS,
} from "../cardFlight.ts";
import type { CardFlightRoute, CardFlightSpec, SweepSpec } from "../cardFlightSpecs.ts";
import {
  CARD_FLIGHT_FACE_READY_MAX_MS,
  CardVfxLayer,
  type CardVfxLayerHandle,
  corsOnlySrc,
  PIXEL_RATIO_CAP,
} from "../CardVfxLayer.tsx";
import { SHATTER_CRACK_S, SHATTER_FALL_S } from "../cardShatter.ts";
import { type CardVfxScene, SCENE_EFFECT_KINDS } from "../cardVfxScene.ts";
import type {
  CardShatterSpec,
  CardVfxSpec,
  CounterRippleSpec,
  DamageBlowSpec,
  DamageStrikeSpec,
  DamageStrikeTarget,
  ExileDissolveSpec,
} from "../cardVfxSpecs.ts";
import { HIT_S } from "../damageStrike.ts";
import { DISSOLVE_CROSS_S, DISSOLVE_LIFT_S, DISSOLVE_TRAVEL_S } from "../exileDissolve.ts";

interface RecordedChild {
  name: string;
  visible: boolean;
  position: [number, number, number];
  object: Object3D;
}

interface RendererCall {
  method: "compileAsync" | "render" | "initTexture" | "setPixelRatio" | "dispose" | "forceContextLoss";
  arg?: unknown;
  children?: RecordedChild[];
}

// A recording stand-in for three's WebGLRenderer (the WebGL boundary): the
// real scene and flight code run on top of it. Constructions are counted
// apart from compile and render calls.
const gl = vi.hoisted(() => ({
  attempts: 0,
  constructed: 0,
  throwOnCreate: false,
  holdCompile: false,
  releaseCompile: [] as Array<() => void>,
  calls: [] as RendererCall[],
}));

vi.mock("three", async (importOriginal) => {
  const three = await importOriginal<typeof import("three")>();
  const snapshot = (scene: Scene): RecordedChild[] =>
    scene.children.map((object) => ({
      name: object.name,
      visible: object.visible,
      position: object.position.toArray() as [number, number, number],
      object,
    }));
  class FakeWebGLRenderer {
    private readonly canvas: HTMLCanvasElement;
    constructor({ canvas }: { canvas: HTMLCanvasElement }) {
      gl.attempts += 1;
      if (gl.throwOnCreate) throw new Error("Error creating WebGL context.");
      gl.constructed += 1;
      this.canvas = canvas;
    }
    setClearColor() {}
    setSize() {}
    setPixelRatio(ratio: number) {
      gl.calls.push({ method: "setPixelRatio", arg: ratio });
    }
    initTexture(texture: Texture) {
      gl.calls.push({ method: "initTexture", arg: texture });
    }
    compileAsync(scene: Scene) {
      gl.calls.push({ method: "compileAsync", children: snapshot(scene) });
      return new Promise<Scene>((resolve) => {
        if (gl.holdCompile) gl.releaseCompile.push(() => resolve(scene));
        else resolve(scene);
      });
    }
    render(scene: Scene) {
      gl.calls.push({ method: "render", children: snapshot(scene) });
    }
    dispose() {
      gl.calls.push({ method: "dispose" });
    }
    forceContextLoss() {
      gl.calls.push({ method: "forceContextLoss" });
      this.canvas.dispatchEvent(new Event("webglcontextlost"));
    }
  }
  return { ...three, WebGLRenderer: FakeWebGLRenderer };
});

vi.mock("../../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn((name: string) => ({
    src: `${name}.png`,
    isLoading: false,
    isRotated: false,
    isFlip: false,
  })),
  // Lazy: the factory is hoisted above `DEFAULT_BACK`.
  useCardBackImage: vi.fn(() => DEFAULT_BACK),
}));

const advanceFailedSource = vi.fn();
const INSTALLED_BACK = {
  kind: "installed",
  src: "back.png",
  assetKey: assetKey("asset:v1:card_back:W10"),
  packId: packId("core"),
  catalogRoot: catalogRoot("a".repeat(64)),
} as const;
// An installed source passes through `corsOnlySrc` unchanged, so `loadBack()`
// finds `img[src="back.png"]`.
const DEFAULT_BACK = { src: "back.png", isLoading: false, source: INSTALLED_BACK, advanceFailedSource };

const X = 7;
const Y = 8;
const FRAME_MS = 16;
const CARD_W = 63;
const CARD_H = 88;

function face(objectId: number, cardName = "Llanowar Elves"): AnimationImageSnapshot {
  return { objectId, cardName, faceIndex: 0, isToken: false };
}

/** The snapshot a step queued now animates: the next to commit. */
function queuedSeq() {
  return useGameStore.getState().lastCommittedSeq + 1;
}

function spec(
  objectId: number,
  route: CardFlightRoute,
  faces: Pick<CardFlightSpec, "startFace" | "endFace"> = { startFace: null, endFace: null },
  owningStepMs = 500,
): CardFlightSpec {
  return {
    kind: "flight",
    objectId,
    sourceId: objectId,
    route,
    ...faces,
    endColors: null,
    pace: 1,
    owningStepMs,
    snapshotSeq: queuedSeq(),
    delayMs: 0,
    wash: null,
    sweep: null,
  };
}

const CAST: CardFlightRoute = { from: "Hand", to: "Stack", ownerId: 0 };
const RESOLVE: CardFlightRoute = { from: "Stack", to: "Battlefield", ownerId: 0 };

/** Adds a laid-out anchor node: happy-dom reports zero size for every node. */
function anchor(attributes: Record<string, string>, left = 40, top = 600) {
  const el = document.createElement("div");
  for (const [name, value] of Object.entries(attributes)) el.setAttribute(name, value);
  Object.defineProperty(el, "offsetWidth", { configurable: true, value: CARD_W });
  Object.defineProperty(el, "offsetHeight", { configurable: true, value: CARD_H });
  el.getBoundingClientRect = () => new DOMRect(left, top, CARD_W, CARD_H);
  document.body.appendChild(el);
  return el;
}

/** Stubs `el`'s layout box: happy-dom reports zero for every node. */
function stubBox(el: HTMLElement, width: number, height: number) {
  Object.defineProperty(el, "offsetWidth", { configurable: true, value: width });
  Object.defineProperty(el, "offsetHeight", { configurable: true, value: height });
}

/** An `<img>` with a stubbed layout box and `complete` flag, added to `parent`.
 *  A face fills its node (63 × 88); a pip is 4 × 4. */
function addImg(parent: HTMLElement, width: number, height: number, complete: boolean) {
  const img = document.createElement("img");
  stubBox(img, width, height);
  Object.defineProperty(img, "complete", { configurable: true, get: () => complete });
  parent.appendChild(img);
  return img;
}

const addFace = (parent: HTMLElement) => addImg(parent, CARD_W, CARD_H, true);
const addPip = (parent: HTMLElement) => addImg(parent, 4, 4, true);

/** What a surface renders while its face loads: a `<div>` with no `<img>`. */
function addPulse(parent: HTMLElement) {
  const pulse = document.createElement("div");
  parent.appendChild(pulse);
  return pulse;
}

const handCard = (objectId: number) =>
  anchor({ "data-hand-card": "", "data-object-id": String(objectId) });

async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function frames(count: number) {
  await advance(count * FRAME_MS);
}

function last<T>(items: readonly T[]): T | undefined {
  return items[items.length - 1];
}

function calls(method: RendererCall["method"]) {
  return gl.calls.filter((call) => call.method === method);
}

const hasVisible = (call: RendererCall, name: string) =>
  call.children?.some((child) => child.name === name && child.visible) ?? false;

const flightRenders = () => calls("render").filter((call) => hasVisible(call, "card-flight"));

function veiled(objectId: number) {
  return useAnimationStore.getState().flightVeiledObjectIds.has(objectId);
}

/** The engine commits the step's state. */
const commitEngine = () =>
  act(() => {
    useGameStore.setState((state) => ({
      engineCommitEpoch: state.engineCommitEpoch + 1,
      lastCommittedSeq: state.lastCommittedSeq + 1,
    }));
  });

function canvas() {
  const element = document.querySelector<HTMLCanvasElement>("canvas[data-card-vfx]");
  if (!element) throw new Error("no overlay canvas");
  return element;
}

function loadBack() {
  const back = document.querySelector<HTMLImageElement>('img[src="back.png"]');
  if (!back) throw new Error("no back loader");
  fireEvent.load(back);
}

function faceLoader(cardName = "Llanowar Elves") {
  return document.querySelector<HTMLImageElement>(`img[src="${cardName}.png"]`);
}

async function renderLayer({ tier = "full", strict = false }: { tier?: "full" | "reduced"; strict?: boolean } = {}) {
  const ref = createRef<CardVfxLayerHandle>();
  const tree = <CardVfxLayer ref={ref} tier={tier} />;
  const utils = render(strict ? <StrictMode>{tree}</StrictMode> : tree);
  await act(async () => {
    await vi.dynamicImportSettled();
  });
  const present = (effect: CardVfxSpec, classic: () => void = vi.fn(), onImpact?: () => void) => {
    act(() => ref.current?.present(effect, classic, onImpact));
    return classic;
  };
  return { ref, present, ...utils };
}

/** Renders the layer and drives it through its first-effect init to ready. */
async function readyLayer(options?: Parameters<typeof renderLayer>[0]) {
  const layer = await renderLayer(options);
  const first = layer.present(spec(99, CAST));
  loadBack();
  // The init frame posts the init task; the fake clock runs a zero-delay
  // timer posted mid-tick 1 ms later.
  await frames(1);
  await advance(1);
  expect(first).toHaveBeenCalledTimes(1);
  expect(gl.constructed).toBe(1);
  return layer;
}

let unveilSpy: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  vi.useFakeTimers({
    toFake: ["setTimeout", "clearTimeout", "requestAnimationFrame", "cancelAnimationFrame", "performance"],
  });
  Object.assign(gl, { attempts: 0, constructed: 0, throwOnCreate: false, holdCompile: false });
  gl.releaseCompile.length = 0;
  gl.calls.length = 0;
  // The file's `afterEach` does not reset a `vi.fn`'s implementation, so an
  // overridden back source would leak into every later test.
  vi.mocked(useCardBackImage).mockReturnValue(DEFAULT_BACK);
  vi.spyOn(HTMLImageElement.prototype, "decode").mockResolvedValue(undefined);
  unveilSpy = vi.spyOn(useAnimationStore.getState(), "unveilFlight");
});

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
  useAnimationStore.getState().clearQueue();
  useGameStore.getState().reset();
  vi.clearAllMocks();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("CardVfxLayer present contract", () => {
  it("V3-1a: before init, present runs Classic once, synchronously, and veils nothing", async () => {
    const { present } = await renderLayer();
    handCard(X);

    const classic = present(spec(X, CAST));

    expect(classic).toHaveBeenCalledTimes(1);
    expect(useAnimationStore.getState().flightVeiledObjectIds.size).toBe(0);
  });

  it("V3-1b: init waits for the back image, then runs from a task the next frame posts; every present until ready is Classic", async () => {
    const rafSpy = vi.spyOn(window, "requestAnimationFrame");
    const { present } = await renderLayer();
    handCard(X);

    expect(present(spec(X, CAST))).toHaveBeenCalledTimes(1);
    // Module loaded, back unsettled: no init frame yet.
    expect(rafSpy).not.toHaveBeenCalled();

    let initFrame: FrameRequestCallback | null = null;
    rafSpy.mockImplementationOnce((callback) => {
      initFrame = callback;
      return 1;
    });
    loadBack();
    expect(rafSpy).toHaveBeenCalledTimes(1);
    expect(initFrame).not.toBeNull();

    gl.holdCompile = true;
    act(() => initFrame?.(0));
    expect(gl.constructed).toBe(0);
    await advance(0);
    expect(gl.constructed).toBe(1);

    expect(present(spec(X, CAST))).toHaveBeenCalledTimes(1);
    expect(veiled(X)).toBe(false);

    await act(async () => {
      for (const release of gl.releaseCompile) release();
    });
    const gl3 = present(spec(X, CAST));
    expect(gl3).not.toHaveBeenCalled();
    expect(veiled(X)).toBe(true);
  });

  it("V3-1c: once ready, a present starts a GL flight and veils its object instead of running Classic", async () => {
    const { present } = await readyLayer();
    // No veil-aware source and no active flight: Classic, once, unveiled.
    const unsourced = present(spec(X, CAST));
    expect(unsourced).toHaveBeenCalledTimes(1);
    expect(veiled(X)).toBe(false);
    handCard(X);

    const classic = present(spec(X, CAST));

    expect(classic).not.toHaveBeenCalled();
    expect(veiled(X)).toBe(true);
    await frames(1);
    expect(flightRenders().length).toBeGreaterThan(0);
  });

  it.each(["full", "reduced"] as const)(
    "V3-1d: at tier %s, init compiles every registered effect kind's warm-up meshes, textured",
    async (tier) => {
      const warmUps = SCENE_EFFECT_KINDS.map((kind) => {
        const returned: Object3D[] = [];
        const original = kind.warmUp.bind(kind);
        vi.spyOn(kind, "warmUp").mockImplementation((host) => {
          const objects = original(host);
          returned.push(...objects);
          return objects;
        });
        return returned;
      });

      await readyLayer({ tier });

      const [compile] = calls("compileAsync");
      const compiled = compile.children?.map((child) => child.object) ?? [];
      for (const returned of warmUps) {
        expect(returned.length).toBeGreaterThan(0);
        for (const object of returned) expect(compiled).toContain(object);
      }
      const names = compile.children?.map((child) => child.name);
      expect(names).toEqual(expect.arrayContaining(["card-flight-warmup", "card-shadow-warmup"]));
      const card = compiled.find((object) => object.name === "card-flight-warmup") as Mesh<never, ShaderMaterial>;
      expect(card.material.uniforms.uFront.value).toMatchObject({ isTexture: true });
      expect(card.material.uniforms.uBack.value).toMatchObject({ isTexture: true });
    },
  );

  it("V3-1e: the back is uploaded once in init, before the warm-up frame, and never by a flight", async () => {
    const { present } = await readyLayer();
    const back = document.querySelector('img[src="back.png"]');
    const backUploads = () =>
      gl.calls.filter((call) => call.method === "initTexture" && (call.arg as Texture).image === back);

    expect(backUploads()).toHaveLength(1);
    const uploadAt = gl.calls.indexOf(backUploads()[0]);
    const warmUpRenderAt = gl.calls.findIndex((call) => call.method === "render");
    expect(uploadAt).toBeLessThan(warmUpRenderAt);

    handCard(X);
    present(spec(X, CAST));
    await frames(2);
    expect(flightRenders().length).toBeGreaterThan(0);
    expect(backUploads()).toHaveLength(1);
  });

  it("V3-1f: a visible face is uploaded before the flight's first frame; a hidden one loads nothing", async () => {
    const { present } = await readyLayer();
    handCard(X);
    const useCardImageCalls = vi.mocked(useCardImage).mock.calls.length;

    // Both faces hidden: no loader, no face upload, no image lookup.
    const hiddenUploads = calls("initTexture").length;
    present(spec(X, CAST));
    expect(faceLoader()).toBeNull();
    await frames(2);
    expect(calls("initTexture")).toHaveLength(hiddenUploads);
    expect(vi.mocked(useCardImage).mock.calls).toHaveLength(useCardImageCalls);

    handCard(Y);
    const classic = present(spec(Y, CAST, { startFace: face(Y), endFace: face(Y) }));
    const loader = faceLoader();
    expect(loader).not.toBeNull();
    await act(async () => {
      if (loader) fireEvent.load(loader);
    });
    const faceUpload = gl.calls.findIndex(
      (call) => call.method === "initTexture" && (call.arg as Texture).image === loader,
    );
    expect(faceUpload).toBeGreaterThanOrEqual(0);
    const rendersBefore = gl.calls.length;
    await frames(1);
    const firstYRender = gl.calls.findIndex(
      (call, index) =>
        index >= rendersBefore &&
        call.method === "render" &&
        (call.children ?? []).filter((child) => child.name === "card-flight").length === 2,
    );
    expect(firstYRender).toBeGreaterThan(faceUpload);
    expect(classic).not.toHaveBeenCalled();
  });

  it("V3-1g (i): a second present for a flying object hands off from its pose with no unveil between", async () => {
    const { present } = await readyLayer();
    handCard(X);
    present(spec(X, CAST));
    await frames(5);
    const before = last(flightRenders());
    const lastPose = before?.children?.find((child) => child.name === "card-flight")?.position;

    // No stack entry exists yet: the resolution continues the active flight.
    const classic = present(spec(X, RESOLVE));
    expect(classic).not.toHaveBeenCalled();
    await frames(1);

    const after = last(flightRenders());
    const flights = after?.children?.filter((child) => child.name === "card-flight") ?? [];
    expect(flights).toHaveLength(1);
    expect(flights[0].position[0]).toBeCloseTo(lastPose?.[0] ?? NaN, 6);
    expect(flights[0].position[1]).toBeCloseTo(lastPose?.[1] ?? NaN, 6);
    expect(flights[0].position[2]).toBeCloseTo(lastPose?.[2] ?? NaN, 6);
    expect(unveilSpy).not.toHaveBeenCalled();
    expect(veiled(X)).toBe(true);
  });

  it("V3-1g (ii): presents that join a pending start make one flight on load, or both run Classic in order at the deadline", async () => {
    const { present } = await readyLayer();
    handCard(X);
    const faces = { startFace: face(X), endFace: face(X) };

    const cast = present(spec(X, CAST, faces));
    const resolve = present(spec(X, RESOLVE, faces));
    const loader = faceLoader();
    await act(async () => {
      if (loader) fireEvent.load(loader);
    });
    await frames(1);
    const flights = last(flightRenders())?.children?.filter((child) => child.name === "card-flight");
    expect(flights).toHaveLength(1);
    expect(cast).not.toHaveBeenCalled();
    expect(resolve).not.toHaveBeenCalled();

    handCard(Y);
    const order: string[] = [];
    present(spec(Y, CAST, { startFace: face(Y, "Grizzly Bears"), endFace: face(Y, "Grizzly Bears") }), () => order.push("cast"));
    present(spec(Y, RESOLVE, { startFace: face(Y, "Grizzly Bears"), endFace: face(Y, "Grizzly Bears") }), () => order.push("resolve"));
    await advance(CARD_FLIGHT_FACE_READY_MAX_MS);
    expect(order).toEqual(["cast", "resolve"]);
    expect(veiled(Y)).toBe(false);
  });

  it("V3-1h: a face that misses its deadline presents Classic once, and a late load starts nothing", async () => {
    const { present } = await readyLayer();
    handCard(X);
    const owningStepMs = 300;
    const deadline = Math.min(CARD_FLIGHT_FACE_READY_MAX_MS, 0.3 * owningStepMs);

    const classic = present(spec(X, CAST, { startFace: face(X), endFace: face(X) }, owningStepMs));
    const loader = faceLoader();
    await advance(deadline - 1);
    expect(classic).not.toHaveBeenCalled();
    await advance(1);
    expect(classic).toHaveBeenCalledTimes(1);
    expect(veiled(X)).toBe(false);

    await act(async () => {
      if (loader) fireEvent.load(loader);
    });
    await frames(2);
    expect(classic).toHaveBeenCalledTimes(1);
    expect(veiled(X)).toBe(false);
    expect(flightRenders()).toHaveLength(0);
  });

  it("V3-1i: a renderer that cannot be created leaves the layer Classic for good", async () => {
    gl.throwOnCreate = true;
    const { present } = await renderLayer();
    handCard(X);
    present(spec(99, CAST));
    loadBack();
    await frames(1);
    await advance(1);
    expect(gl.attempts).toBe(1);

    for (let i = 0; i < 3; i += 1) {
      expect(present(spec(X, CAST))).toHaveBeenCalledTimes(1);
      await frames(1);
    }
    expect(gl.attempts).toBe(1);
    expect(veiled(X)).toBe(false);
  });

  it("V3-1j: a lost context releases veils and presents Classic; a restore re-warms the same renderer", async () => {
    const { present } = await readyLayer();
    handCard(X);
    present(spec(X, CAST));
    await frames(2);
    expect(veiled(X)).toBe(true);

    act(() => {
      canvas().dispatchEvent(new Event("webglcontextlost"));
    });
    expect(unveilSpy).toHaveBeenCalledWith(X);
    expect(veiled(X)).toBe(false);
    expect(present(spec(X, CAST))).toHaveBeenCalledTimes(1);

    const callsBeforeRestore = gl.calls.length;
    gl.holdCompile = true;
    act(() => {
      canvas().dispatchEvent(new Event("webglcontextrestored"));
    });
    const restored = gl.calls.slice(callsBeforeRestore);
    expect(restored.map((call) => call.method)).toEqual(["initTexture", "compileAsync"]);
    expect(present(spec(X, CAST))).toHaveBeenCalledTimes(1);

    await act(async () => {
      for (const release of gl.releaseCompile) release();
    });
    const warmUpRender = gl.calls.slice(callsBeforeRestore).find((call) => call.method === "render");
    const warmUpMeshes = warmUpRender?.children?.filter((child) => child.name.endsWith("-warmup")) ?? [];
    expect(warmUpMeshes.length).toBeGreaterThan(0);
    expect(warmUpMeshes.every((child) => child.visible)).toBe(true);

    expect(present(spec(X, CAST))).not.toHaveBeenCalled();
    expect(veiled(X)).toBe(true);
    expect(gl.constructed).toBe(1);
  });

  it("V3-1k: many flights share the one renderer", async () => {
    const { present } = await readyLayer();
    for (let id = 10; id < 16; id += 1) {
      handCard(id);
      expect(present(spec(id, CAST))).not.toHaveBeenCalled();
      await frames(3);
    }
    expect(gl.constructed).toBe(1);
  });

  it("V3-1l: the frame loop runs while a flight is active and stops, hidden, once it ends", async () => {
    const { present } = await readyLayer();
    handCard(X);
    const rafSpy = vi.spyOn(window, "requestAnimationFrame");
    present(spec(X, CAST));
    await frames(3);
    const whileActive = rafSpy.mock.calls.length;
    await frames(1);
    expect(rafSpy.mock.calls.length).toBeGreaterThan(whileActive);
    expect(canvas().style.visibility).toBe("visible");

    // The commit shows the own node with its face already loaded; the flight lands and ends.
    addFace(anchor({ "data-stack-entry": String(X) }, 700, 200));
    commitEngine();
    await advance(RESOLVE_FLIGHT_MS + 10 * FRAME_MS);
    expect(veiled(X)).toBe(false);
    const idle = rafSpy.mock.calls.length;
    await frames(10);
    expect(rafSpy.mock.calls.length).toBe(idle);
    expect(canvas().style.visibility).toBe("hidden");
  });

  it("unmount releases retained warm-up resources exactly once", async () => {
    const { unmount } = await readyLayer();
    const objects: Array<Mesh | Points> = [];
    for (const root of calls("compileAsync")[0].children!) {
      root.object.traverse((object) => {
        if ((object as Mesh).isMesh || (object as Points).isPoints) objects.push(object as Mesh | Points);
      });
    }
    const geometries = new Set(objects.map((object) => object.geometry));
    const materials = new Set(objects.flatMap((object) => Array.isArray(object.material) ? object.material : [object.material]));
    const card = objects.find((object) => object.name === "card-flight-warmup") as Mesh<never, ShaderMaterial>;
    const textures = new Set<Texture>([card.material.uniforms.uFront.value, card.material.uniforms.uBack.value]);
    const disposals = [...geometries, ...materials, ...textures].map((resource) => vi.spyOn(resource, "dispose"));
    for (const dispose of disposals) expect(dispose).not.toHaveBeenCalled();
    unmount();
    for (const dispose of disposals) expect(dispose).toHaveBeenCalledTimes(1);
  });

  it("unmount during context restoration stops the pending warm-up", async () => {
    const { unmount } = await readyLayer();
    act(() => canvas().dispatchEvent(new Event("webglcontextlost")));
    gl.holdCompile = true;
    act(() => canvas().dispatchEvent(new Event("webglcontextrestored")));
    expect(gl.releaseCompile).toHaveLength(1);
    unmount();
    const renders = calls("render").length;
    await act(async () => {
      for (const release of gl.releaseCompile) release();
    });
    expect(calls("render")).toHaveLength(renders);
    expect(calls("dispose")).toHaveLength(1);
    expect(useAnimationStore.getState().cardVfxReady).toBe(false);
  });

  it("V3-1m: unmount runs each waiting Classic once, releases each veil once, and nothing after", async () => {
    const { present, unmount } = await readyLayer();
    handCard(X);
    handCard(Y);
    present(spec(X, CAST));
    await frames(1);
    const waiting = present(spec(Y, CAST, { startFace: face(Y), endFace: face(Y) }));
    expect(veiled(X)).toBe(true);

    unmount();

    expect(waiting).toHaveBeenCalledTimes(1);
    expect(unveilSpy).toHaveBeenCalledTimes(1);
    expect(unveilSpy).toHaveBeenCalledWith(X);
    expect(calls("dispose")).toHaveLength(1);
    expect(calls("forceContextLoss")).toHaveLength(1);

    await advance(CARD_FLIGHT_FACE_READY_MAX_MS + 10 * FRAME_MS);
    expect(waiting).toHaveBeenCalledTimes(1);
    expect(unveilSpy).toHaveBeenCalledTimes(1);
  });

  it("V3-1n (i): a resolution with no own node after the commit abandons: one unveil, then the GL card fades out", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(X) }, 700, 200);
    present(spec(X, RESOLVE));
    await frames(2);
    expect(veiled(X)).toBe(true);

    act(() => {
      useGameStore.setState((state) => ({
        engineCommitEpoch: state.engineCommitEpoch + 1,
        lastCommittedSeq: state.lastCommittedSeq + 1,
      }));
    });
    await frames(2);
    expect(veiled(X)).toBe(false);
    expect(unveilSpy).toHaveBeenCalledTimes(1);
    expect(unveilSpy).toHaveBeenCalledWith(X);

    const fadeStart = gl.calls.length;
    await frames(2);
    expect(gl.calls.slice(fadeStart).some((call) => call.method === "render" && hasVisible(call, "card-flight"))).toBe(true);
    await advance(ABANDON_FADE_MS);
    const afterFade = gl.calls.length;
    await frames(3);
    expect(gl.calls.slice(afterFade).some((call) => hasVisible(call, "card-flight"))).toBe(false);
    expect(canvas().style.visibility).toBe("hidden");
    expect(unveilSpy).toHaveBeenCalledTimes(1);
  });

  it("V3-1n (ii): a resolution onto its own card-shaped slot stays veiled until it lands and settles", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(X) }, 700, 200);
    present(spec(X, RESOLVE));
    await frames(1);
    anchor({ "data-permanent-card": String(X) }, 300, 400);
    act(() => {
      useGameStore.setState((state) => ({
        engineCommitEpoch: state.engineCommitEpoch + 1,
        lastCommittedSeq: state.lastCommittedSeq + 1,
      }));
    });

    await advance(RESOLVE_FLIGHT_MS / 2);
    expect(veiled(X)).toBe(true);
    // Past t = 1 (the leg's end) but inside the settle.
    await advance(RESOLVE_FLIGHT_MS / 2 + 2 * FRAME_MS);
    expect(veiled(X)).toBe(true);
    expect(unveilSpy).not.toHaveBeenCalled();

    await advance(SETTLE_MS + 3 * FRAME_MS);
    expect(veiled(X)).toBe(false);
    expect(unveilSpy).toHaveBeenCalledTimes(1);
    expect(unveilSpy).toHaveBeenCalledWith(X);
  });

  it.each([
    ["full", true],
    ["reduced", false],
  ] as const)("V4-1: a battlefield landing at tier %s kicks up dust: %s", async (tier, dusty) => {
    const { present } = await readyLayer({ tier });
    anchor({ "data-stack-entry": String(X) }, 700, 200);
    present(spec(X, RESOLVE));
    await frames(1);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 400));
    act(() => {
      useGameStore.setState((state) => ({
        engineCommitEpoch: state.engineCommitEpoch + 1,
        lastCommittedSeq: state.lastCommittedSeq + 1,
      }));
    });
    await advance(RESOLVE_FLIGHT_MS + SETTLE_MS + 5 * FRAME_MS);
    expect(veiled(X)).toBe(false);

    const dustRenders = calls("render").filter((call) => hasVisible(call, "landing-dust"));
    expect(dustRenders.length > 0).toBe(dusty);
    // One landing, one dust cloud; it clears itself and the loop goes idle.
    for (const call of dustRenders) {
      expect(call.children?.filter((child) => child.name === "landing-dust")).toHaveLength(1);
    }
    await advance(1000);
    expect(hasVisible(last(calls("render")) as RendererCall, "landing-dust")).toBe(false);
    expect(canvas().style.visibility).toBe("hidden");
  });

  it("V5-3: a draw leaves the library after its stagger and lands in the veiled hand slot", async () => {
    const { present } = await readyLayer();
    anchor({ "data-library-pile": "0" }, 40, 700);
    const draw = { ...spec(X, { from: "Library", to: "Hand", ownerId: 0 }), delayMs: 100 };
    present(draw);
    await frames(2);
    // Veiled at once, so the hand never shows the card before its flight;
    // until its turn it rests on the pile it leaves from.
    expect(veiled(X)).toBe(true);
    const flightAt = () => last(flightRenders())?.children?.find((child) => child.name === "card-flight")?.position;
    const pile = flightAt();
    expect(pile?.[1]).toBeCloseTo(-(700 + CARD_H / 2), 0);
    await advance(100 + 5 * FRAME_MS);
    expect(flightAt()).not.toEqual(pile);

    addFace(anchor({ "data-hand-card": "", "data-object-id": String(X) }, 200, 760));
    act(() => {
      useGameStore.setState((state) => ({
        engineCommitEpoch: state.engineCommitEpoch + 1,
        lastCommittedSeq: state.lastCommittedSeq + 1,
      }));
    });
    await advance(DRAW_FLIGHT_MS + LAND_STATIONARY_WAIT_MAX_MS + 10 * FRAME_MS);
    expect(veiled(X)).toBe(false);
    expect(unveilSpy).toHaveBeenCalledWith(X);
  });

  it("V4-2: casts and graveyard landings kick up no dust", async () => {
    const { present } = await readyLayer();
    handCard(X);
    present(spec(X, CAST));
    await frames(3);
    addFace(anchor({ "data-stack-entry": String(X) }, 700, 200));
    commitEngine();
    await advance(CAST_FLIGHT_MS + 10 * FRAME_MS);
    expect(veiled(X)).toBe(false);

    addFace(anchor({ "data-graveyard-pile": "0", "data-grouped-ids": String(Y) }, 40, 700));
    anchor({ "data-stack-entry": String(Y) }, 700, 200);
    present(spec(Y, { from: "Stack", to: "Graveyard", ownerId: 0 }));
    commitEngine();
    await advance(RESOLVE_FLIGHT_MS + SETTLE_MS + 10 * FRAME_MS);
    expect(veiled(Y)).toBe(false);

    expect(calls("render").some((call) => hasVisible(call, "landing-dust"))).toBe(false);
  });

  it("V11-7: a token leaves from its source's surface, and with no source surface presents Classic", async () => {
    const { present } = await readyLayer();
    const elf = face(X, "Elf Warrior");
    const token = { ...spec(X, RESOLVE, { startFace: elf, endFace: elf }), sourceId: Y };
    expect(present(token)).toHaveBeenCalledTimes(1);

    anchor({ "data-stack-entry": String(Y) }, 700, 200);
    const classic = present(token);
    await act(async () => {
      const loader = faceLoader("Elf Warrior");
      if (loader) fireEvent.load(loader);
    });
    await frames(1);

    expect(classic).not.toHaveBeenCalled();
    expect(veiled(X)).toBe(true);
    const card = flightRenders()[0]?.children?.find((child) => child.name === "card-flight");
    expect(card?.position[0]).toBeCloseTo(700 + CARD_W / 2);
    expect(card?.position[1]).toBeCloseTo(-(200 + CARD_H / 2));
  });

  it.each([
    ["Library", { "data-library-pile": "1" }],
    ["Exile", { "data-exile-pile": "1" }],
  ] as const)("V11-9: a card flies into its owner's %s pile and ends there, its old surface veiled until the commit", async (to, pile) => {
    const { present } = await readyLayer();
    handCard(X);
    anchor(pile, 40, 700);

    present(spec(X, { from: "Hand", to, ownerId: 1 }));
    await frames(1);
    expect(veiled(X)).toBe(true);

    // Well inside the await bound, so only a landing (not an abandon) ends it.
    await advance(RESOLVE_FLIGHT_MS + LAND_STATIONARY_WAIT_MAX_MS + LAND_REVEAL_WAIT_MAX_MS + 10 * FRAME_MS);
    expect(canvas().style.visibility).toBe("hidden");
    // The hand card is still in hand until the commit moves it.
    expect(veiled(X)).toBe(true);
    commitEngine();
    expect(veiled(X)).toBe(false);
    expect(unveilSpy).toHaveBeenCalledTimes(1);
  });

  it("V11-10: a flight that starts while a landed flight waits on the commit keeps the veil past it", async () => {
    const { present } = await readyLayer();
    handCard(X);
    anchor({ "data-exile-pile": "1" }, 40, 700);
    present(spec(X, { from: "Hand", to: "Exile", ownerId: 1 }));
    await advance(RESOLVE_FLIGHT_MS + LAND_STATIONARY_WAIT_MAX_MS + LAND_REVEAL_WAIT_MAX_MS + 10 * FRAME_MS);
    expect(veiled(X)).toBe(true);

    present(spec(X, { from: "Exile", to: "Hand", ownerId: 1 }));
    await frames(1);
    commitEngine();
    expect(veiled(X)).toBe(true);
    expect(unveilSpy).not.toHaveBeenCalled();
  });

  it("V11-11: a flight whose commit lands while its face loads ends on its own node without another commit", async () => {
    const { present } = await readyLayer();
    handCard(X);
    const classic = present(spec(X, CAST, { startFace: face(X), endFace: face(X) }));

    commitEngine();
    addFace(anchor({ "data-stack-entry": String(X) }, 700, 200));
    await act(async () => {
      const loader = faceLoader();
      if (loader) fireEvent.load(loader);
    });
    await advance(CAST_FLIGHT_MS + LAND_STATIONARY_WAIT_MAX_MS + LAND_REVEAL_WAIT_MAX_MS + 10 * FRAME_MS);

    expect(classic).not.toHaveBeenCalled();
    expect(veiled(X)).toBe(false);
  });

  it("V11-12: a flight whose step's commit landed before it was presented ends without another commit", async () => {
    const { present } = await readyLayer();
    handCard(X);
    anchor({ "data-exile-pile": "1" }, 40, 700);
    const queued = spec(X, { from: "Hand", to: "Exile", ownerId: 1 });

    commitEngine();
    present(queued);
    await advance(RESOLVE_FLIGHT_MS + LAND_STATIONARY_WAIT_MAX_MS + LAND_REVEAL_WAIT_MAX_MS + 10 * FRAME_MS);

    expect(veiled(X)).toBe(false);
  });

  it("V11-13: a flight whose step's snapshot a newer one superseded before it was queued ends without a commit", async () => {
    const { present } = await readyLayer();
    handCard(X);
    anchor({ "data-exile-pile": "1" }, 40, 700);

    commitEngine();
    commitEngine();

    // The step animates the snapshot before the committed one.
    const classic = present({ ...spec(X, { from: "Hand", to: "Exile", ownerId: 1 }), snapshotSeq: queuedSeq() - 2 });
    await frames(1);
    expect(veiled(X)).toBe(true);
    await advance(RESOLVE_FLIGHT_MS + LAND_STATIONARY_WAIT_MAX_MS + LAND_REVEAL_WAIT_MAX_MS + 10 * FRAME_MS);

    expect(classic).not.toHaveBeenCalled();
    expect(veiled(X)).toBe(false);
  });

  it("V11-8: a covered event shows nothing while its move flies, waits on one still loading, and is Classic otherwise", async () => {
    const { present } = await renderLayer();
    expect(present({ kind: "covered", objectId: X })).toHaveBeenCalledTimes(1);
    loadBack();
    await frames(1);
    await advance(1);
    expect(gl.constructed).toBe(1);

    // No move of its own presented: Classic.
    expect(present({ kind: "covered", objectId: X })).toHaveBeenCalledTimes(1);
    // A flight under way holds the veil.
    handCard(X);
    present(spec(X, CAST));
    expect(present({ kind: "covered", objectId: X })).not.toHaveBeenCalled();

    // A flight still loading its face: the covered Classic runs only if it falls back.
    anchor({ "data-permanent-card": String(Y) }, 300, 400);
    const move = present(spec(Y, { from: "Battlefield", to: "Hand", ownerId: 0 }, { startFace: face(Y), endFace: face(Y) }));
    const waiting = present({ kind: "covered", objectId: Y });
    expect(waiting).not.toHaveBeenCalled();
    await advance(CARD_FLIGHT_FACE_READY_MAX_MS + 10);
    expect(move).toHaveBeenCalledTimes(1);
    expect(waiting).toHaveBeenCalledTimes(1);
  });

  it("V3-1n (iii): a released card stays drawn until the destination shows its face", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(X) }, 700, 200);
    present(spec(X, RESOLVE));
    await frames(1);
    // The destination renders a pulse where its face will be, beside a
    // complete pip: only the face-sized image counts.
    const permanent = anchor({ "data-permanent-card": String(X) }, 300, 400);
    const pulse = addPulse(permanent);
    addPip(permanent);
    act(() => {
      useGameStore.setState((state) => ({
        engineCommitEpoch: state.engineCommitEpoch + 1,
        lastCommittedSeq: state.lastCommittedSeq + 1,
      }));
    });

    await advance(RESOLVE_FLIGHT_MS + SETTLE_MS + 5 * FRAME_MS);
    expect(unveilSpy).toHaveBeenCalledTimes(1);
    expect(unveilSpy).toHaveBeenCalledWith(X);
    for (let i = 0; i < 3; i += 1) {
      await frames(1);
      expect(hasVisible(last(calls("render")) as RendererCall, "card-flight")).toBe(true);
    }

    // The face arrives, as the surface renders it once its source resolves.
    pulse.remove();
    addFace(permanent);
    await frames(2);
    expect(hasVisible(last(calls("render")) as RendererCall, "card-flight")).toBe(false);
    // The landing dust outlives the card; the overlay hides once it clears.
    await advance(1000);
    expect(canvas().style.visibility).toBe("hidden");
  });

  it("V3-1q: the back is requested under a CORS-only URL, and an installed back is left alone", async () => {
    const R = CARD_BACK_URL;
    const rewritten = new URL(corsOnlySrc({ kind: "remote", src: R }));
    expect(rewritten.searchParams.get("cors")).toBe("1");
    expect(rewritten.origin).toBe(new URL(R).origin);
    expect(rewritten.pathname).toBe(new URL(R).pathname);
    expect(rewritten.toString()).not.toBe(R);
    const withQuery = new URL(corsOnlySrc({ kind: "remote", src: `${R}?v=2` }));
    expect(withQuery.searchParams.get("v")).toBe("2");
    expect(withQuery.searchParams.get("cors")).toBe("1");
    expect(corsOnlySrc({ ...INSTALLED_BACK, src: "blob:pack-back" })).toBe("blob:pack-back");

    const source = { kind: "remote", src: R } as const;
    vi.mocked(useCardBackImage).mockReturnValue({ src: R, isLoading: false, source, advanceFailedSource });
    await renderLayer();
    const sources = [...document.querySelectorAll("img")].map((img) => img.getAttribute("src"));
    expect(sources).toContain(corsOnlySrc(source));
    expect(sources).not.toContain(R);
    expect(document.querySelector(`img[src="${corsOnlySrc(source)}"]`)?.getAttribute("crossorigin")).toBe(
      "anonymous",
    );
  });

  it("V3-1p: a failed back advances the ladder with its own URL; with no back, flips run Classic and other flights run GL", async () => {
    const R = CARD_BACK_URL;
    const source = { kind: "remote", src: R } as const;
    vi.mocked(useCardBackImage).mockReturnValue({ src: R, isLoading: false, source, advanceFailedSource });
    const failed = await renderLayer();
    const back = document.querySelector<HTMLImageElement>(`img[src="${corsOnlySrc(source)}"]`);
    if (!back) throw new Error("no back loader");
    fireEvent.error(back);
    expect(advanceFailedSource).toHaveBeenCalledTimes(1);
    expect(advanceFailedSource).toHaveBeenCalledWith(R);
    failed.unmount();

    // The ladder is exhausted: the loader settles `null` and init runs.
    vi.mocked(useCardBackImage).mockReturnValue({ src: null, isLoading: false, source: null, advanceFailedSource });
    const { present } = await renderLayer();
    present(spec(99, CAST));
    await frames(1);
    await advance(1);
    expect(gl.constructed).toBe(1);

    handCard(X);
    const flip = present(spec(X, CAST, { startFace: null, endFace: face(X) }));
    expect(flip).toHaveBeenCalledTimes(1);
    expect(veiled(X)).toBe(false);
    // A flight that needs no back still flies.
    handCard(Y);
    const still = present(spec(Y, CAST, { startFace: face(Y, "Grizzly Bears"), endFace: face(Y, "Grizzly Bears") }));
    const loader = faceLoader("Grizzly Bears");
    await act(async () => {
      if (loader) fireEvent.load(loader);
    });
    expect(veiled(Y)).toBe(true);
    expect(still).not.toHaveBeenCalled();
  });

  it("V3-1p: with the remote back loaded, a flip flight runs GL", async () => {
    const source = { kind: "remote", src: CARD_BACK_URL } as const;
    vi.mocked(useCardBackImage).mockReturnValue({
      src: CARD_BACK_URL,
      isLoading: false,
      source,
      advanceFailedSource,
    });
    const { present } = await renderLayer();
    present(spec(99, CAST));
    const back = document.querySelector<HTMLImageElement>(`img[src="${corsOnlySrc(source)}"]`);
    if (!back) throw new Error("no back loader");
    fireEvent.load(back);
    await frames(1);
    await advance(1);
    expect(gl.constructed).toBe(1);

    handCard(X);
    const flip = present(spec(X, CAST, { startFace: null, endFace: face(X) }));
    const loader = faceLoader();
    await act(async () => {
      if (loader) fireEvent.load(loader);
    });
    expect(flip).not.toHaveBeenCalled();
    expect(veiled(X)).toBe(true);
    await frames(1);
    expect(flightRenders().length).toBeGreaterThan(0);
  });

  /** A ready layer with a cast of X released onto a stack entry whose art is
   *  still loading: a pulse beside a complete pip, so the cast's card is
   *  revealing. Returns the scene runtime its `warmUp` received. */
  async function revealingCast() {
    const warmUp = vi.spyOn(SCENE_EFFECT_KINDS[0], "warmUp");
    const layer = await readyLayer();
    // The scene runtime is the `host` init lends to each effect kind's warm-up.
    const scene = warmUp.mock.calls[0]?.[0] as unknown as CardVfxScene | undefined;
    handCard(X);
    layer.present(spec(X, CAST));
    const entry = anchor({ "data-stack-entry": String(X) }, 700, 200);
    addPulse(entry);
    addPip(entry);
    commitEngine();
    await advance(CAST_FLIGHT_MS + 5 * FRAME_MS);
    expect(unveilSpy).toHaveBeenCalledWith(X);
    await frames(2);
    // The hold is live: the card is still drawn over the loading entry.
    expect(hasVisible(last(calls("render")) as RendererCall, "card-flight")).toBe(true);
    if (!scene) throw new Error("no scene");
    return { ...layer, scene };
  }

  it("V3-1r: a GL resolution replaces a revealing cast with exactly one card", async () => {
    const { present, scene } = await revealingCast();
    expect(scene.hasFlight(X)).toBe(false);

    const classic = present(spec(X, RESOLVE));
    await frames(1);

    const flights = last(flightRenders())?.children?.filter((child) => child.name === "card-flight" && child.visible);
    expect(flights).toHaveLength(1);
    expect(veiled(X)).toBe(true);
    expect(classic).not.toHaveBeenCalled();
    expect(scene.hasFlight(X)).toBe(true);
  });

  it("V3-1u: a Classic resolution ends a revealing cast's card, but not before its face deadline", async () => {
    const { present } = await revealingCast();
    const deadline = Math.min(CARD_FLIGHT_FACE_READY_MAX_MS, 0.3 * 500);

    // The face never fires `load`, so this present waits for its deadline.
    const classic = present(spec(X, RESOLVE, { startFace: null, endFace: face(X) }));
    await advance(deadline - 1);
    expect(classic).not.toHaveBeenCalled();
    expect(hasVisible(last(calls("render")) as RendererCall, "card-flight")).toBe(true);

    await advance(1);
    expect(classic).toHaveBeenCalledTimes(1);
    const afterClassic = gl.calls.length;
    await frames(2);
    expect(gl.calls.slice(afterClassic).some((call) => hasVisible(call, "card-flight"))).toBe(false);
    expect(canvas().style.visibility).toBe("hidden");
    expect(veiled(X)).toBe(false);
  });

  it("V3-1o: a StrictMode mount → unmount → mount still initialises one renderer and presents GL", async () => {
    const { present } = await readyLayer({ strict: true });
    handCard(X);

    const classic = present(spec(X, CAST));

    expect(classic).not.toHaveBeenCalled();
    expect(veiled(X)).toBe(true);
    await frames(1);
    expect(flightRenders().length).toBeGreaterThan(0);
    expect(gl.constructed).toBe(1);
  });
});

describe("CardVfxLayer tiers", () => {
  function pointer(coarse: boolean) {
    vi.stubGlobal("matchMedia", (query: string) => ({
      matches: query === "(pointer: coarse)" && coarse,
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
    }));
  }

  it.each([
    [false, "full", PIXEL_RATIO_CAP.fine],
    [true, "full", PIXEL_RATIO_CAP.coarse],
    [false, "reduced", PIXEL_RATIO_CAP.reduced],
    [true, "reduced", PIXEL_RATIO_CAP.reduced],
  ] as const)("V3-10a: coarse pointer %s at tier %s caps the pixel ratio at %s", async (coarse, tier, cap) => {
    vi.stubGlobal("devicePixelRatio", 3);
    pointer(coarse);

    await readyLayer({ tier });

    expect(last(calls("setPixelRatio"))?.arg).toBe(cap);
    expect(PIXEL_RATIO_CAP.coarse).toBeLessThan(PIXEL_RATIO_CAP.fine);
  });

  it("V3-10a: a tier change re-applies the cap on the same renderer", async () => {
    vi.stubGlobal("devicePixelRatio", 3);
    pointer(false);
    const { ref, rerender } = await readyLayer({ tier: "full" });
    expect(last(calls("setPixelRatio"))?.arg).toBe(PIXEL_RATIO_CAP.fine);

    rerender(<CardVfxLayer ref={ref} tier="reduced" />);

    expect(last(calls("setPixelRatio"))?.arg).toBe(PIXEL_RATIO_CAP.reduced);
    expect(gl.constructed).toBe(1);
  });

  it.each([
    ["full", true],
    ["reduced", false],
  ] as const)("V3-10b: a flight at tier %s has a shadow: %s", async (tier, shadowed) => {
    const { present } = await readyLayer({ tier });
    handCard(X);
    present(spec(X, CAST));
    await frames(1);

    const [first] = flightRenders();
    expect(first).toBeDefined();
    expect(hasVisible(first, "card-shadow")).toBe(shadowed);
  });
});

describe("CardVfxLayer shatter", () => {
  const SHATTER_MS = (SHATTER_CRACK_S + SHATTER_FALL_S) * 1000 + 5 * FRAME_MS + 100;
  const shatter = (objectId: number, cardFace: AnimationImageSnapshot | null = face(objectId)): CardShatterSpec => ({
    kind: "shatter", destroyerId: null,
    objectId,
    face: cardFace,
    pace: 1,
    owningStepMs: 500,
    snapshotSeq: queuedSeq(),
    sweep: null,
  });
  const shatterRenders = () => calls("render").filter((call) => hasVisible(call, "card-shatter"));
  const commit = () =>
    act(() => {
      useGameStore.setState((state) => ({
        engineCommitEpoch: state.engineCommitEpoch + 1,
        lastCommittedSeq: state.lastCommittedSeq + 1,
      }));
    });

  async function loadFace() {
    const loader = faceLoader();
    expect(loader).not.toBeNull();
    await act(async () => {
      if (loader) fireEvent.load(loader);
    });
  }

  it("V8-1: a destroyed permanent breaks where it lies, veiled until it has shattered and the commit has landed", async () => {
    const { present } = await readyLayer();
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 400));

    const classic = present(shatter(X));
    // Still on the board while its face loads.
    expect(veiled(X)).toBe(false);
    await loadFace();

    expect(classic).not.toHaveBeenCalled();
    expect(veiled(X)).toBe(true);
    const surfaceUpload = last(calls("initTexture"));
    expect((surfaceUpload?.arg as Texture).image).toBeInstanceOf(HTMLCanvasElement);
    await frames(2);
    expect(shatterRenders().length).toBeGreaterThan(0);
    const group = shatterRenders()[0].children?.find((child) => child.name === "card-shatter");
    expect(group?.position.slice(0, 2)).toEqual([300 + CARD_W / 2, -(400 + CARD_H / 2)]);

    // The shatter ends before the commit: the board card stays hidden.
    await advance(SHATTER_MS);
    expect(hasVisible(last(calls("render")) as RendererCall, "card-shatter")).toBe(false);
    expect(veiled(X)).toBe(true);
    commit();
    expect(veiled(X)).toBe(false);
  });

  it("V8-1: a commit before the shatter ends releases the veil when it ends", async () => {
    const { present } = await readyLayer();
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 400));
    present(shatter(X));
    await loadFace();
    commit();
    await frames(2);
    expect(veiled(X)).toBe(true);

    await advance(SHATTER_MS);
    expect(veiled(X)).toBe(false);
  });

  it("V8-2: no permanent, no face shown, or a face that fails to load presents Classic and veils nothing", async () => {
    const { present } = await readyLayer();

    expect(present(shatter(X))).toHaveBeenCalledTimes(1);
    const permanent = anchor({ "data-permanent-card": String(X) }, 300, 400);
    // A permanent still loading its face has nothing to break.
    expect(present(shatter(X))).toHaveBeenCalledTimes(1);
    addFace(permanent);
    expect(present(shatter(X, null))).toHaveBeenCalledTimes(1);

    const late = present(shatter(X));
    expect(late).not.toHaveBeenCalled();
    await advance(CARD_FLIGHT_FACE_READY_MAX_MS);
    expect(late).toHaveBeenCalledTimes(1);
    expect(veiled(X)).toBe(false);
    expect(shatterRenders()).toHaveLength(0);
  });

  it("V8-11: in art-crop mode a card loads its art crop and a token its full image, as the board shows them", async () => {
    usePreferencesStore.setState({ battlefieldCardDisplay: "art_crop" });
    const { present } = await readyLayer();
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 400));
    addFace(anchor({ "data-permanent-card": String(Y) }, 500, 400));
    const sizes = () => vi.mocked(useCardImage).mock.calls.map(([name, options]) => [name, options?.size]);

    present(shatter(X));
    present(shatter(Y, { ...face(Y, "Soldier"), isToken: true }));

    expect(sizes()).toEqual(expect.arrayContaining([["Llanowar Elves", "art_crop"], ["Soldier", "normal"]]));
  });

  it("V11-8: a covered event waits on a board effect loading its face: dropped if it starts, Classic if it falls back", async () => {
    const { present } = await readyLayer();
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 400));
    present(shatter(X));
    const dropped = present({ kind: "covered", objectId: X });
    await loadFace();
    expect(dropped).not.toHaveBeenCalled();

    addFace(anchor({ "data-permanent-card": String(Y) }, 500, 400));
    const failed = present(shatter(Y));
    const waiting = present({ kind: "covered", objectId: Y });
    await advance(CARD_FLIGHT_FACE_READY_MAX_MS + 10);
    expect(failed).toHaveBeenCalledTimes(1);
    expect(waiting).toHaveBeenCalledTimes(1);
  });

  it("V8-3: unmounting mid-shatter releases the veil", async () => {
    const { present, unmount } = await readyLayer();
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 400));
    present(shatter(X));
    await loadFace();
    expect(veiled(X)).toBe(true);

    unmount();
    expect(veiled(X)).toBe(false);
  });
});

describe("CardVfxLayer exile dissolve", () => {
  const dissolve = (objectId: number, holderId: number | null = null): ExileDissolveSpec => ({
    kind: "dissolve",
    look: "exile",
    objectId,
    face: face(objectId),
    holderId,
    pace: 1,
    owningStepMs: 500,
    snapshotSeq: queuedSeq(),
  });

  async function loadFace() {
    const loader = faceLoader();
    expect(loader).not.toBeNull();
    await act(async () => {
      if (loader) fireEvent.load(loader);
    });
  }

  it("V9-6: a held card shows as its ghost once its flakes arrive and the commit lands", async () => {
    const { present } = await readyLayer();
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 400));
    anchor({ "data-permanent-card": "3" }, 500, 400);

    const classic = present(dissolve(X, 3));
    await loadFace();
    expect(classic).not.toHaveBeenCalled();
    expect(veiled(X)).toBe(true);
    await frames(2);
    expect(calls("render").some((call) => hasVisible(call, "exile-dissolve"))).toBe(true);

    anchor({ "data-exile-ghost": String(X) }, 510, 420);
    act(() => {
      useGameStore.setState((state) => ({
        engineCommitEpoch: state.engineCommitEpoch + 1,
        lastCommittedSeq: state.lastCommittedSeq + 1,
      }));
    });
    await advance((DISSOLVE_LIFT_S + DISSOLVE_CROSS_S + DISSOLVE_TRAVEL_S * 0.85) * 1000 + 5 * FRAME_MS);
    expect(veiled(X)).toBe(false);
  });

  it("V9-6: a permanent with no face on the board presents Classic", async () => {
    const { present } = await readyLayer();
    anchor({ "data-permanent-card": String(X) }, 300, 400);
    expect(present(dissolve(X))).toHaveBeenCalledTimes(1);
    expect(veiled(X)).toBe(false);
  });

  it("V9-8: a flight that takes over a dissolving card keeps the veil past the dissolve's end and the commit", async () => {
    const { present } = await readyLayer();
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 400));
    present(dissolve(X));
    await loadFace();
    expect(veiled(X)).toBe(true);

    // A back-only flight to a hand with no card of X's: it holds until the commit.
    const classic = present(spec(X, { from: "Battlefield", to: "Hand", ownerId: 1 }));
    // With no holder, the dissolve arrives as it ends.
    await advance((DISSOLVE_LIFT_S + DISSOLVE_CROSS_S + DISSOLVE_TRAVEL_S * 1.3) * 1000 + 5 * FRAME_MS);
    commitEngine();

    expect(classic).not.toHaveBeenCalled();
    expect(veiled(X)).toBe(true);
    expect(unveilSpy).not.toHaveBeenCalled();
  });
});

describe("CardVfxLayer damage strike", () => {
  const SPELL = 20;
  const strike = (target: DamageStrikeTarget): DamageStrikeSpec => ({
    kind: "damage", snapshotSeq: queuedSeq(),
    cause: "fire",
    origin: { zone: "Stack", objectId: SPELL, ownerId: 0 },
    target,
    amount: 3,
    pace: 1,
    owningStepMs: 900,
  });
  const atPlayer = strike({ kind: "player", playerId: 1 });
  const atPermanent = (cardFace: AnimationImageSnapshot | null = face(X)) =>
    strike({ kind: "permanent", objectId: X, face: cardFace });
  const strikeRenders = () => calls("render").filter((call) => hasVisible(call, "damage-strike"));
  const hitRenders = () => calls("render").filter((call) => hasVisible(call, "damage-hit"));

  async function loadFace() {
    const loader = faceLoader();
    expect(loader).not.toBeNull();
    await act(async () => {
      if (loader) fireEvent.load(loader);
    });
  }

  it("V10-7: a strike at a player leaves the stack entry now and lands once, at the impact", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(SPELL) }, 600, 300);
    anchor({ "data-player-hud": "1" }, 400, 20);
    const onImpact = vi.fn();

    const classic = present(atPlayer, vi.fn(), onImpact);
    await frames(2);
    expect(strikeRenders().length).toBeGreaterThan(0);
    await advance(DAMAGE_CAUSE_IMPACT_MS - 4 * FRAME_MS);
    expect(onImpact).not.toHaveBeenCalled();
    await advance(4 * FRAME_MS);
    expect(onImpact).toHaveBeenCalledTimes(1);
    expect(classic).not.toHaveBeenCalled();
  });

  it("V10-8: a struck permanent is veiled from the impact until its copy has rocked back to rest", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(SPELL) }, 600, 300);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 100));
    const onImpact = vi.fn();

    const classic = present(atPermanent(), vi.fn(), onImpact);
    await loadFace();
    await advance(DAMAGE_CAUSE_IMPACT_MS - 4 * FRAME_MS);
    expect(veiled(X)).toBe(false);
    expect(hitRenders()).toHaveLength(0);
    // The strike's clock starts on its first frame.
    await advance(6 * FRAME_MS);
    expect(onImpact).toHaveBeenCalledTimes(1);
    expect(veiled(X)).toBe(true);
    expect(hitRenders().length).toBeGreaterThan(0);

    await advance(HIT_S * 1000 + 2 * FRAME_MS);
    expect(veiled(X)).toBe(false);
    expect(classic).not.toHaveBeenCalled();
  });

  it("V10-9: no source, no target, or a target face that is not shown presents Classic and never lands", async () => {
    const { present } = await readyLayer();
    const onImpact = vi.fn();
    anchor({ "data-player-hud": "1" }, 400, 20);
    expect(present(atPlayer, vi.fn(), onImpact)).toHaveBeenCalledTimes(1);

    anchor({ "data-stack-entry": String(SPELL) }, 600, 300);
    expect(present(atPermanent(), vi.fn(), onImpact)).toHaveBeenCalledTimes(1);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 100));
    expect(present(atPermanent(null), vi.fn(), onImpact)).toHaveBeenCalledTimes(1);

    await advance(DAMAGE_CAUSE_IMPACT_MS * 2);
    expect(onImpact).not.toHaveBeenCalled();
    expect(strikeRenders()).toHaveLength(0);
  });

  it("V10-13: a hit taking over from a running hit shows the permanent at rest until its own impact", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(SPELL) }, 600, 300);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 100));
    present(atPermanent());
    await loadFace();
    await advance(DAMAGE_CAUSE_IMPACT_MS + 2 * FRAME_MS);
    expect(veiled(X)).toBe(true);

    present(atPermanent());
    await loadFace();
    await frames(2);
    expect(veiled(X)).toBe(true);
    expect(hasVisible(last(calls("render")) as RendererCall, "damage-hit")).toBe(true);

    // The first hit never completes, so the second releases the veil it inherited.
    await advance(DAMAGE_CAUSE_IMPACT_MS + HIT_S * 1000 + 4 * FRAME_MS);
    expect(veiled(X)).toBe(false);
  });

  it("V10-14: a collapsed group member with no card of its own presents Classic", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(SPELL) }, 600, 300);
    addFace(anchor({ "data-permanent-card": String(Y), "data-grouped-ids": `${Y} ${X}` }, 300, 100));

    expect(present(atPermanent())).toHaveBeenCalledTimes(1);
    expect(present({ kind: "shatter", destroyerId: null, objectId: X, face: face(X), pace: 1, owningStepMs: 400, snapshotSeq: queuedSeq(), sweep: null })).toHaveBeenCalledTimes(1);
  });

  it("V10-15: unmounting before the impact lands the hit once and releases the veil it takes", async () => {
    const { present, unmount } = await readyLayer();
    anchor({ "data-stack-entry": String(SPELL) }, 600, 300);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 100));
    const onImpact = vi.fn();
    present(atPermanent(), vi.fn(), onImpact);
    await loadFace();
    await frames(2);

    unmount();
    expect(onImpact).toHaveBeenCalledTimes(1);
    expect(veiled(X)).toBe(false);
  });

  it("V17-6: a consecutive knockback keeps a resting, occluded copy visible until its new impact", async () => {
    const { present } = await readyLayer();
    anchor({ "data-permanent-card": String(X) }, 40, 400);
    addFace(anchor({ "data-permanent-card": String(Y) }, 300, 400));
    const knockback = (): CardVfxSpec => ({
      kind: "knockback", objectId: Y, face: face(Y), sourceId: X,
      amount: 3, pace: 1, owningStepMs: 900, startMs: performance.now(), impactDelayMs: 200,
    });
    const load = async () => {
      const loader = faceLoader();
      expect(loader).not.toBeNull();
      await act(async () => { if (loader) fireEvent.load(loader); });
    };
    present(knockback());
    await load();
    await advance(900);
    expect(veiled(Y)).toBe(true);
    const classic = present(knockback());
    await load();
    await frames(2);
    const copy = last(calls("render"))?.children?.find((child) => child.name === "damage-hit" && child.visible)?.object;
    expect(copy).toBeDefined();
    const uniforms = ((copy!.children[0] as Mesh).material as ShaderMaterial).uniforms;
    expect(uniforms.uPush.value.length()).toBe(0);
    expect(uniforms.uOccHalf.value.x).toBe(CARD_W / 2);
    expect(veiled(Y)).toBe(true);
    await advance(200 + HIT_S * 1000 + 2 * FRAME_MS);
    expect(veiled(Y)).toBe(false);
    expect(classic).not.toHaveBeenCalled();
  });

  it("V12-5: a blow lands dust on a player's HUD or a permanent at once, with no face to load and no veil", async () => {
    const { present } = await readyLayer();
    anchor({ "data-permanent-card": String(X) }, 40, 600);
    anchor({ "data-player-hud": "1" }, 400, 40);
    anchor({ "data-permanent-card": String(Y) }, 300, 400);
    const blow = (target: DamageBlowSpec["target"]): DamageBlowSpec => ({
      kind: "blow",
      sourceId: X,
      target,
      amount: 3,
      pace: 1,
      startMs: performance.now(),
      impactDelayMs: 300,
    });

    for (const target of [{ Player: 1 }, { Object: Y }]) {
      const classic = present(blow(target));
      expect(classic).not.toHaveBeenCalled();
    }
    await frames(1);
    const blows = last(calls("render"))?.children?.filter((child) => child.name === "damage-blow");
    expect(blows).toHaveLength(2);
    expect(veiled(Y)).toBe(false);
  });

  it("V16-7: a struck permanent stays itself until the slam lands, then its copy rocks back in its place", async () => {
    const { present } = await readyLayer();
    anchor({ "data-permanent-card": String(X) }, 40, 400);
    addFace(anchor({ "data-permanent-card": String(Y) }, 300, 400));
    const classic = present({
      kind: "knockback",
      objectId: Y,
      face: face(Y),
      sourceId: X,
      amount: 3,
      pace: 1,
      owningStepMs: 500,
      startMs: performance.now(),
      impactDelayMs: 200,
    });
    const loader = faceLoader();
    await act(async () => {
      if (loader) fireEvent.load(loader);
    });
    await frames(2);
    expect(veiled(Y)).toBe(false);
    await advance(200);
    expect(veiled(Y)).toBe(true);
    expect(hasVisible(last(calls("render")) as RendererCall, "damage-hit")).toBe(true);
    await advance(HIT_S * 1000 + 2 * FRAME_MS);
    expect(veiled(Y)).toBe(false);
    expect(classic).not.toHaveBeenCalled();
  });

  it("V12-5: a blow with no surface to land on, or at pace 0, presents Classic", async () => {
    const { present } = await readyLayer();
    const blow: DamageBlowSpec = { kind: "blow", sourceId: null, target: { Object: Y }, amount: 3, pace: 1, startMs: performance.now(), impactDelayMs: 300 };
    expect(present(blow)).toHaveBeenCalledTimes(1);
    anchor({ "data-permanent-card": String(Y) }, 300, 400);
    expect(present({ ...blow, pace: 0 })).toHaveBeenCalledTimes(1);
    expect(present(blow)).not.toHaveBeenCalled();
  });

  it("V13-4: life and counter changes play over the HUD or the permanent now, veiling nothing", async () => {
    const { present } = await readyLayer();
    anchor({ "data-player-hud": "1" }, 400, 40);
    anchor({ "data-permanent-card": String(Y) }, 300, 400);

    const life = present({ kind: "life", playerId: 1, amount: 3, pace: 1 });
    const counter = present({ kind: "counter", objectId: Y, counterType: "P1P1", change: "added", count: 1, pace: 1 });
    await frames(1);

    expect(life).not.toHaveBeenCalled();
    expect(counter).not.toHaveBeenCalled();
    const names = last(calls("render"))?.children?.map((child) => child.name);
    expect(names).toEqual(expect.arrayContaining(["life-change", "counter-change"]));
    expect(veiled(Y)).toBe(false);
  });

  it("V13-6: a counter or blow on a collapsed group's member plays over the group's representative", async () => {
    const { present } = await readyLayer();
    anchor({ "data-permanent-card": String(X), "data-grouped-ids": `${X} ${Y}` }, 300, 400);

    const counter = present({ kind: "counter", objectId: Y, counterType: "P1P1", change: "added", count: 1, pace: 1 });
    const blow = present({ kind: "blow", sourceId: null, target: { Object: Y }, amount: 3, pace: 1, startMs: performance.now(), impactDelayMs: 300 });
    await frames(1);

    expect(counter).not.toHaveBeenCalled();
    expect(blow).not.toHaveBeenCalled();
    const names = last(calls("render"))?.children?.map((child) => child.name);
    expect(names).toEqual(expect.arrayContaining(["counter-change", "damage-blow"]));
    expect(veiled(X)).toBe(false);
  });

  it("V13-4: a life or counter change with nowhere to play, or at pace 0, presents Classic", async () => {
    const { present } = await readyLayer();
    expect(present({ kind: "life", playerId: 1, amount: 3, pace: 1 })).toHaveBeenCalledTimes(1);
    expect(present({ kind: "counter", objectId: Y, counterType: "P1P1", change: "added", count: 1, pace: 1 })).toHaveBeenCalledTimes(1);
    anchor({ "data-player-hud": "1" }, 400, 40);
    expect(present({ kind: "life", playerId: 1, amount: 3, pace: 0 })).toHaveBeenCalledTimes(1);
  });

  it("V10-16: readiness is published: not before init, while ready, not after unmount", async () => {
    const { unmount } = await renderLayer();
    expect(useAnimationStore.getState().cardVfxReady).toBe(false);
    unmount();

    const ready = await readyLayer();
    expect(useAnimationStore.getState().cardVfxReady).toBe(true);
    ready.unmount();
    expect(useAnimationStore.getState().cardVfxReady).toBe(false);
  });

  it("V17-1: a fire-struck permanent burns on a later SBA death and stays veiled for the burn", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(SPELL) }, 600, 300);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 100));
    present(atPermanent());
    await loadFace();
    await advance(DAMAGE_CAUSE_IMPACT_MS + 2 * FRAME_MS);
    expect(veiled(X)).toBe(true);

    present({ kind: "shatter", destroyerId: null, objectId: X, face: face(X), pace: 1, owningStepMs: 400, snapshotSeq: queuedSeq(), sweep: null });
    await loadFace();
    await frames(2);
    expect(hasVisible(last(calls("render")) as RendererCall, "damage-hit")).toBe(false);
    expect(hasVisible(last(calls("render")) as RendererCall, "card-burn")).toBe(true);
    await advance(HIT_S * 1000);
    expect(veiled(X)).toBe(true);
  });

  it.each([
    { label: "a different batch", seqOffset: 1, destroyerId: null, cause: "fire" as const },
    { label: "a destroy instruction", seqOffset: 0, destroyerId: SPELL, cause: "fire" as const },
    { label: "lightning damage", seqOffset: 0, destroyerId: null, cause: "lightning" as const },
  ])("V17-2: $label shatters rather than borrowing a fire hit", async ({ seqOffset, destroyerId, cause }) => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(SPELL) }, 600, 300);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 100));
    const snapshotSeq = queuedSeq();
    present({ ...atPermanent(), cause, snapshotSeq });
    await loadFace();
    await advance(DAMAGE_CAUSE_IMPACT_MS + 2 * FRAME_MS);
    present({ kind: "shatter", destroyerId, objectId: X, face: face(X), pace: 1, owningStepMs: 400, snapshotSeq: snapshotSeq + seqOffset, sweep: null });
    await loadFace();
    await frames(2);
    expect(hasVisible(last(calls("render")) as RendererCall, "card-shatter")).toBe(true);
    expect(hasVisible(last(calls("render")) as RendererCall, "card-burn")).toBe(false);
  });

  it("V10-20: a hit replacing one still before its impact stays hidden, with the permanent unveiled, until its own", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(SPELL) }, 600, 300);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 100));
    present(atPermanent());
    await loadFace();
    present(atPermanent());
    await loadFace();
    await frames(2);

    expect(veiled(X)).toBe(false);
    expect(hasVisible(last(calls("render")) as RendererCall, "damage-hit")).toBe(false);
  });

  it("V10-18: a strike whose hit was replaced lands after the replacement ends without veiling again", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(SPELL) }, 600, 300);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 100));
    const slowImpact = vi.fn();
    present({ ...atPermanent(), pace: 2 }, vi.fn(), slowImpact);
    await loadFace();
    present({ ...atPermanent(), pace: 0.1 });
    await loadFace();

    await advance((DAMAGE_CAUSE_IMPACT_MS + HIT_S * 1000) * 0.1 + 4 * FRAME_MS);
    expect(veiled(X)).toBe(false);
    await advance(DAMAGE_CAUSE_IMPACT_MS * 2);
    expect(slowImpact).toHaveBeenCalledTimes(1);
    expect(veiled(X)).toBe(false);
  });

  it("V10-19: a hit replacing a hit leaves a flight's hold on the card alone", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(SPELL) }, 600, 300);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 100));
    present(atPermanent());
    await loadFace();
    await advance(DAMAGE_CAUSE_IMPACT_MS + 2 * FRAME_MS);
    // A back-only flight to a hand with no card of X's: it holds until the commit.
    const classic = present(spec(X, { from: "Battlefield", to: "Hand", ownerId: 1 }));
    await frames(1);

    present(atPermanent());
    await loadFace();
    await advance(DAMAGE_CAUSE_IMPACT_MS + HIT_S * 1000 + 4 * FRAME_MS);

    expect(classic).not.toHaveBeenCalled();
    expect(veiled(X)).toBe(true);
    expect(unveilSpy).not.toHaveBeenCalled();
  });

  it("V10-17: a shatter that takes over before the strike's impact keeps the veil, and releases it", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(SPELL) }, 600, 300);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 100));
    const onImpact = vi.fn();
    present(atPermanent(), vi.fn(), onImpact);
    await loadFace();
    present({ kind: "shatter", destroyerId: null, objectId: X, face: face(X), pace: 1, owningStepMs: 400, snapshotSeq: queuedSeq(), sweep: null });
    await loadFace();
    expect(veiled(X)).toBe(true);

    await advance((SHATTER_CRACK_S + SHATTER_FALL_S) * 1000 + 10 * FRAME_MS);
    expect(onImpact).toHaveBeenCalledTimes(1);
    expect(veiled(X)).toBe(true);
    commitEngine();
    expect(veiled(X)).toBe(false);
  });
});

describe("CardVfxLayer counter ripple", () => {
  const COUNTER = 20;
  const ripple: CounterRippleSpec = {
    kind: "ripple",
    origin: { zone: "Stack", objectId: COUNTER, ownerId: 0 },
    targetId: X,
    look: "water",
    pace: 1,
    leaveMs: 600,
  };
  const rippleRenders = () => calls("render").filter((call) => hasVisible(call, "counter-ripple"));
  const TO_GRAVEYARD: CardFlightRoute = { from: "Stack", to: "Graveyard", ownerId: 0 };

  it("V14-5: a ripple runs between the two stack entries, veiling neither; with either missing it presents Classic", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(X) }, 700, 200);
    expect(present(ripple)).toHaveBeenCalledTimes(1);

    anchor({ "data-stack-entry": String(COUNTER) }, 700, 320);
    const classic = present(ripple);
    await frames(2);
    expect(rippleRenders().length).toBeGreaterThan(0);
    expect(veiled(X) || veiled(COUNTER)).toBe(false);
    expect(classic).not.toHaveBeenCalled();
  });

  it("V14-6: a countered spell's wait and wash count from its presentation, however long its face takes", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(X) }, 700, 200);
    addFace(anchor({ "data-graveyard-pile": "0", "data-grouped-ids": String(X) }, 40, 700));
    const leaving = {
      ...spec(X, TO_GRAVEYARD, { startFace: face(X), endFace: face(X) }),
      delayMs: 300,
      wash: { atMs: 100, durationMs: 100, look: "water" as const },
    };
    present(leaving);
    await advance(80);
    const loader = faceLoader();
    await act(async () => {
      if (loader) fireEvent.load(loader);
    });
    await frames(2);
    const flight = () => last(flightRenders())?.children?.find((child) => child.name === "card-flight");
    const resting = flight()?.position;
    // Half way through its wash, 150 ms after it was presented.
    await advance(150 - 80 - 2 * FRAME_MS);
    const uniforms = (flight()?.object as Mesh<never, ShaderMaterial>).material.uniforms;
    expect(uniforms.uWash.value).toBeGreaterThan(0.2);
    expect(uniforms.uWash.value).toBeLessThan(0.8);
    // Still resting just before 300 ms; on its way just after.
    await advance(300 - 150 - 2 * FRAME_MS);
    expect(flight()?.position).toEqual(resting);
    await advance(5 * FRAME_MS);
    expect(flight()?.position).not.toEqual(resting);
  });

  it("V14-8: the counter leaves only once its ripple's wash is done, though its move comes a step later", async () => {
    const { present } = await readyLayer();
    anchor({ "data-stack-entry": String(X) }, 700, 200);
    anchor({ "data-stack-entry": String(COUNTER) }, 700, 320);
    addFace(anchor({ "data-graveyard-pile": "0", "data-grouped-ids": String(COUNTER) }, 40, 700));
    present(ripple);
    // The countered spell's step ends; the counter's move is the next step's.
    await advance(400);
    present(spec(COUNTER, TO_GRAVEYARD, { startFace: face(COUNTER), endFace: face(COUNTER) }));
    const loader = faceLoader();
    await act(async () => {
      if (loader) fireEvent.load(loader);
    });
    await frames(2);
    const flight = () => last(flightRenders())?.children?.find((child) => child.name === "card-flight");
    const resting = flight()?.position;
    // Still resting just before 600 ms from the ripple; on its way just after.
    await advance(600 - 400 - 4 * FRAME_MS);
    expect(flight()?.position).toEqual(resting);
    await advance(5 * FRAME_MS);
    expect(flight()?.position).not.toEqual(resting);
  });
});

describe("CardVfxLayer board sweep", () => {
  const sweep: SweepSpec = { look: "light", casterId: 1, memberIds: [X, Y] };
  const swept = (objectId: number, cardName: string): CardShatterSpec => ({
    kind: "shatter", destroyerId: null,
    objectId,
    face: face(objectId, cardName),
    pace: 1,
    owningStepMs: 400,
    snapshotSeq: queuedSeq(),
    sweep,
  });
  const lastRender = () => last(calls("render")) as RendererCall;
  const fronts = () => (lastRender().children ?? []).filter((child) => child.name === "board-sweep" && child.visible);
  /** How far the shatter of the permanent laid out at `top` has cracked. */
  const cracked = (top: number) => {
    const group = lastRender().children?.find(
      (child) => child.name === "card-shatter" && child.position[1] === -(top + CARD_H / 2),
    );
    const shards = group?.object.getObjectByName("card-shatter-shards") as Mesh<never, ShaderMaterial> | undefined;
    return shards?.material.uniforms.uProg.value as number | undefined;
  };

  /** Loads every requested image of each named card. */
  async function loadFaces(...names: string[]) {
    await act(async () => {
      for (const name of names) {
        for (const loader of document.querySelectorAll(`img[src="${name}.png"]`)) fireEvent.load(loader);
      }
    });
  }

  it("V15-5: one front sets out from the caster's side and breaks each permanent as it reaches it", async () => {
    const { present } = await readyLayer();
    anchor({ "data-player-hud": "1" }, 400, 20);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 450));
    addFace(anchor({ "data-permanent-card": String(Y) }, 300, 150));
    present(swept(X, "Llanowar Elves"));
    present(swept(Y, "Hill Giant"));
    await loadFaces("Llanowar Elves", "Hill Giant");
    await frames(2);
    expect(fronts()).toHaveLength(1);
    // Both lie whole, veiled, until the front comes down from the HUD above.
    expect(veiled(X) && veiled(Y)).toBe(true);
    expect(cracked(150)).toBe(0);
    expect(cracked(450)).toBe(0);
    // The nearer card breaks first; the farther one still lies whole.
    await advance(300);
    expect(cracked(150)).toBeGreaterThan(0);
    expect(cracked(450)).toBe(0);
    await advance(400);
    expect(cracked(450)).toBeGreaterThan(0);
  });

  it("V15-5: a wave sends each card home as it reaches it", async () => {
    const { present } = await readyLayer();
    const TO_HAND: CardFlightRoute = { from: "Battlefield", to: "Hand", ownerId: 0 };
    anchor({ "data-player-hud": "0" }, 400, 700);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 450));
    addFace(anchor({ "data-permanent-card": String(Y) }, 300, 150));
    const wave: SweepSpec = { look: "tsunami", casterId: 0, memberIds: [X, Y] };
    for (const [objectId, cardName] of [[X, "Llanowar Elves"], [Y, "Hill Giant"]] as const) {
      const faces = { startFace: face(objectId, cardName), endFace: face(objectId, cardName) };
      present({ ...spec(objectId, TO_HAND, faces), sweep: wave });
    }
    // Each loads its face, and its art-crop tile to rest as while it waits.
    const sizes = vi.mocked(useCardImage).mock.calls.map(([name, options]) => [name, options?.size]);
    expect(sizes).toEqual(expect.arrayContaining([["Hill Giant", "normal"], ["Hill Giant", "art_crop"]]));
    await loadFaces("Llanowar Elves", "Hill Giant");
    await frames(2);
    expect(fronts()).toHaveLength(1);
    const shown = (name: string) =>
      (lastRender().children ?? []).filter((child) => child.name === name && child.visible).map((child) => child.position.slice(0, 2));
    const lower = [300 + CARD_W / 2, -(450 + CARD_H / 2)];
    const upper = [300 + CARD_W / 2, -(150 + CARD_H / 2)];
    expect(shown("card-flight-rest")).toEqual([lower, upper]);
    expect(shown("card-flight")).toHaveLength(0);
    // From the HUD below, the wave reaches the lower card first: it has left
    // as a card, and the upper one still rests as its tile.
    await advance(300);
    expect(shown("card-flight-rest")).toEqual([upper]);
    expect(shown("card-flight")).toHaveLength(1);
  });

  it("V16-2: destructions one snapshot reports in two steps sweep a front over each", async () => {
    const { present } = await readyLayer();
    anchor({ "data-player-hud": "1" }, 400, 20);
    const W = 9;
    const Z = 10;
    for (const [objectId, top] of [[X, 450], [Y, 150], [W, 300], [Z, 600]]) {
      addFace(anchor({ "data-permanent-card": String(objectId) }, objectId < W ? 300 : 500, top));
    }
    present(swept(X, "Llanowar Elves"));
    present(swept(Y, "Hill Giant"));
    await loadFaces("Llanowar Elves", "Hill Giant");
    await frames(2);
    expect(fronts()).toHaveLength(1);
    // Some other event split the wipe; its second step has members of its own.
    await advance(400);
    const second = { ...sweep, memberIds: [W, Z] };
    present({ ...swept(W, "Serra Angel"), sweep: second });
    present({ ...swept(Z, "Air Elemental"), sweep: second });
    await loadFaces("Serra Angel", "Air Elemental");
    await frames(2);
    expect(fronts()).toHaveLength(2);
    expect(cracked(300)).toBe(0);
  });

  it("V16-3: a face that loads after the front has passed still breaks from its first crack", async () => {
    const { present } = await readyLayer();
    anchor({ "data-player-hud": "1" }, 400, 20);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 450));
    addFace(anchor({ "data-permanent-card": String(Y) }, 300, 150));
    // At half pace the front reaches the nearer card in about 70 ms.
    present({ ...swept(X, "Llanowar Elves"), pace: 0.5 });
    present({ ...swept(Y, "Hill Giant"), pace: 0.5 });
    await loadFaces("Llanowar Elves");
    await advance(110);
    await loadFaces("Hill Giant");
    await frames(1);
    expect(cracked(150)).toBeLessThan(0.05);
  });

  it("V15-5: with fewer than two members on the board there is no front, and the one there breaks at once", async () => {
    const { present } = await readyLayer();
    anchor({ "data-player-hud": "1" }, 400, 20);
    addFace(anchor({ "data-permanent-card": String(X) }, 300, 450));
    present(swept(X, "Llanowar Elves"));
    expect(present(swept(Y, "Hill Giant"))).toHaveBeenCalledTimes(1);
    await loadFaces("Llanowar Elves");
    await frames(2);
    await advance(100);
    expect(fronts()).toHaveLength(0);
    expect(cracked(450)).toBeGreaterThan(0);
  });
});
