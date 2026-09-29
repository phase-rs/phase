import { afterEach, describe, expect, it } from "vitest";

import { objectAnchorSelector } from "../../../../utils/objectAnchorSelector.ts";
import {
  castSourceElement,
  faceImagesSettled,
  measureCardPose,
  measureSurface,
  ownNode,
  provisionalNode,
  resolveAim,
  stackSourceElement,
} from "../cardAnchors.ts";

const X = 7;
const ORIGIN = new DOMRect(0, 0, 1000, 800);

interface Box {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Lays `el` out: happy-dom reports zero size and position for every node. */
function layOut(el: HTMLElement, { left, top, width, height }: Box) {
  Object.defineProperty(el, "offsetWidth", { configurable: true, value: width });
  Object.defineProperty(el, "offsetHeight", { configurable: true, value: height });
  el.getBoundingClientRect = () => new DOMRect(left, top, width, height);
}

function mount(attributes: Record<string, string>, box: Box | null = { left: 0, top: 0, width: 63, height: 88 }, parent: HTMLElement = document.body) {
  const el = document.createElement("div");
  for (const [name, value] of Object.entries(attributes)) el.setAttribute(name, value);
  if (box) layOut(el, box);
  parent.appendChild(el);
  return el;
}

afterEach(() => {
  document.body.replaceChildren();
});

describe("zone-scoped anchors", () => {
  it("V3-5a: one id in four zones resolves to each route's own zone node", () => {
    const hand = mount({ "data-hand-card": "", "data-object-id": String(X) });
    const stack = mount({ "data-stack-entry": String(X), "data-object-id": String(X) });
    const pile = mount({ "data-graveyard-pile": "1", "data-grouped-ids": `3 ${X}` });
    const permanent = mount({ "data-permanent-card": String(X), "data-object-id": String(X) });
    // Positive control: the generic selector really does collide.
    expect(document.querySelectorAll(objectAnchorSelector(X)).length).toBeGreaterThan(1);

    expect(castSourceElement(X)).toBe(hand);
    expect(stackSourceElement(X)).toBe(stack);
    expect(ownNode({ kind: "cast" }, X)).toBe(stack);
    expect(ownNode({ kind: "resolveToBattlefield" }, X)).toBe(permanent);
    expect(ownNode({ kind: "resolveToGraveyard", ownerId: 1 }, X)).toBe(pile);
  });

  it("V3-5a: a coalesced stack entry stands in for its member", () => {
    const representative = mount({ "data-stack-entry": "9", "data-grouped-ids": `9 ${X}` });

    expect(ownNode({ kind: "cast" }, X)).toBe(representative);
    expect(stackSourceElement(X)).toBe(representative);
  });

  it("V3-5b: a held (zero-width) hand card, command-zone and drawer ids, and a pending stack entry are no cast source", () => {
    mount({ "data-hand-card": "", "data-object-id": String(X) }, { left: 0, top: 0, width: 0, height: 88 });
    mount({ "data-object-id": String(X), "data-command-zone": "" });
    mount({ "data-object-id": String(X), "data-mobile-hand-drawer": "" });
    mount({ "data-stack-entry": String(X) });

    expect(castSourceElement(X)).toBeNull();

    // Reach guard: a laid-out hand card is found.
    const visibleHand = mount({ "data-hand-card": "", "data-object-id": String(X) });
    expect(castSourceElement(X)).toBe(visibleHand);
  });

  it("V3-5b: the cast sources are tried in priority order", () => {
    const library = mount({ "data-library-pile": "0" });
    const top = mount({ "data-grouped-ids": String(X) }, undefined, library);
    expect(castSourceElement(X)).toBe(top);
    const pile = mount({ "data-graveyard-pile": "0", "data-grouped-ids": String(X) });
    expect(castSourceElement(X)).toBe(pile);
    const opponent = mount({ "data-opponent-hand-card": String(X) });
    expect(castSourceElement(X)).toBe(opponent);
    const fan = mount({ "data-zone-fan-card": "", "data-object-id": String(X) });
    expect(castSourceElement(X)).toBe(fan);
  });

  it("V3-5c: of two permanent nodes the first laid out one wins, in document order", () => {
    const collapsed = mount({ "data-permanent-card": String(X) }, { left: 0, top: 0, width: 0, height: 0 });
    const overview = mount({ "data-permanent-card": String(X) });
    expect(ownNode({ kind: "resolveToBattlefield" }, X)).toBe(overview);

    layOut(collapsed, { left: 5, top: 5, width: 63, height: 88 });
    expect(ownNode({ kind: "resolveToBattlefield" }, X)).toBe(collapsed);
  });

  it("V3-5d: provisional aims stand in until the own node exists", () => {
    const origin = ORIGIN;
    const hold = resolveAim({ kind: "cast" }, X, origin, null);
    expect(hold).toEqual({ kind: "hold" });

    const remembered = { x: 1, y: 2, w: 63, h: 88, angleDeg: 0 };
    expect(resolveAim({ kind: "cast" }, X, origin, remembered))
      .toEqual({ kind: "provisional", el: null, pose: remembered });
    // Only the cast route remembers a stack pose.
    expect(resolveAim({ kind: "resolveToBattlefield" }, X, origin, remembered)).toEqual({ kind: "hold" });

    mount({ "data-stack-entry": "1" });
    const last = mount({ "data-stack-entry": "2" });
    expect(provisionalNode({ kind: "cast" })).toBe(last);
    expect(resolveAim({ kind: "cast" }, X, origin, remembered)).toMatchObject({ kind: "provisional", el: last });

    const pile = mount({ "data-graveyard-pile": "1", "data-grouped-ids": "3" });
    const route = { kind: "resolveToGraveyard", ownerId: 1 } as const;
    expect(resolveAim(route, X, origin, null)).toMatchObject({ kind: "provisional", el: pile });
    expect(ownNode(route, X)).toBeNull();
    pile.setAttribute("data-grouped-ids", `3 ${X}`);
    expect(resolveAim(route, X, origin, null)).toMatchObject({ kind: "own", el: pile });
  });

  it("V3-5e: a pose is canvas-local, scaled and rotated by its ancestors", () => {
    const scaled = mount({}, null);
    scaled.style.transform = "scale(1.5)";
    const rotated = mount({ "data-permanent-card": String(X) }, { left: 110, top: 220, width: 60, height: 80 }, scaled);
    rotated.style.transform = "rotate(30deg)";

    const pose = measureCardPose(rotated, new DOMRect(10, 20, 500, 500));

    expect(pose.x).toBeCloseTo(110 + 30 - 10, 6);
    expect(pose.y).toBeCloseTo(220 + 40 - 20, 6);
    expect(pose.w).toBeCloseTo(60 * 1.5, 6);
    expect(pose.h).toBeCloseTo(80 * 1.5, 6);
    expect(pose.angleDeg).toBeCloseTo(30, 2);
  });
});

/** An `<img>` in `parent` with a stubbed layout box and `complete` flag. */
function mountImg(parent: HTMLElement, width: number, height: number, complete: boolean) {
  const img = document.createElement("img");
  Object.defineProperty(img, "offsetWidth", { configurable: true, value: width });
  Object.defineProperty(img, "offsetHeight", { configurable: true, value: height });
  Object.defineProperty(img, "complete", { configurable: true, get: () => complete });
  parent.appendChild(img);
  return img;
}

describe("surface measurement", () => {
  it("V3-5f: opacity is the product over the node and its ancestors, and own aims carry it", () => {
    const parent = mount({}, null);
    parent.style.opacity = "0.5";
    const node = mount({ "data-stack-entry": String(X) }, { left: 0, top: 0, width: 63, height: 88 }, parent);
    node.style.opacity = "0.8";

    const surface = measureSurface(node, ORIGIN);
    expect(surface.opacity).toBeCloseTo(0.4, 9);
    expect(surface.pose).toEqual(measureCardPose(node, ORIGIN));

    // An unstyled chain reads 1: happy-dom computes an empty opacity for
    // <body> and <html>, which is the initial value.
    const plain = mount({ "data-permanent-card": String(X) });
    expect(measureSurface(plain, ORIGIN).opacity).toBe(1);

    const own = resolveAim({ kind: "cast" }, X, ORIGIN, null);
    expect(own).toMatchObject({ kind: "own", el: node, faceImagesSettled: false });
    expect(own.kind === "own" && own.opacity).toBeCloseTo(0.4, 9);
    // A provisional aim carries neither field.
    node.removeAttribute("data-stack-entry");
    mount({ "data-stack-entry": "9" });
    const provisional = resolveAim({ kind: "cast" }, X, ORIGIN, null);
    expect(provisional.kind).toBe("provisional");
    expect(provisional).not.toHaveProperty("opacity");
    expect(provisional).not.toHaveProperty("faceImagesSettled");
  });

  it("V3-5g: only face-sized images count towards a settled face", () => {
    const node = mount({}, { left: 0, top: 0, width: 63, height: 88 });

    // (d) No image at all is not settled.
    expect(faceImagesSettled(node)).toBe(false);
    // (a) A pulse <div> beside a complete pip is not settled.
    node.appendChild(document.createElement("div"));
    mountImg(node, 4, 4, true);
    expect(faceImagesSettled(node)).toBe(false);
    // (b) A complete face beside an incomplete pip is settled.
    mountImg(node, 63, 88, true);
    mountImg(node, 4, 4, false);
    expect(faceImagesSettled(node)).toBe(true);
  });

  it("V3-5g: the smallest art-crop face, and several faces", () => {
    // (c) The art-crop geometry: a 58.7 × 44 node with a 44.7 × 24 face.
    const artCrop = mount({}, { left: 0, top: 0, width: 58.7, height: 44 });
    const art = mountImg(artCrop, 44.7, 24, true);
    expect(faceImagesSettled(artCrop)).toBe(true);
    art.remove();
    mountImg(artCrop, 44.7, 24, false);
    expect(faceImagesSettled(artCrop)).toBe(false);

    // (e) Every face must be complete.
    const node = mount({}, { left: 0, top: 0, width: 63, height: 88 });
    mountImg(node, 63, 88, true);
    const second = mountImg(node, 63, 88, false);
    expect(faceImagesSettled(node)).toBe(false);
    second.remove();
    mountImg(node, 63, 88, true);
    expect(faceImagesSettled(node)).toBe(true);
  });
});
