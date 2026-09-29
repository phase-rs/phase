import type { ObjectId } from "../../../adapter/types.ts";
import type { CardFlightRoute } from "./cardFlightSpecs.ts";

/** A card's on-screen pose in canvas-local CSS px: centre, laid-out size with
 *  every ancestor scale applied, and clockwise rotation in degrees. */
export interface CardPose {
  x: number;
  y: number;
  w: number;
  h: number;
  angleDeg: number;
}

/** What a flight aims at this frame. Only an `own` node — the flying object's
 *  own surface in its destination zone — may be landed on; it also reports its
 *  accumulated `opacity` (a fading or entering container) and whether its face
 *  image has `faceImagesSettled`. A `provisional` aim with no element is a
 *  remembered pose. A `hold` aim has no DOM target; the flight hovers above
 *  its source. */
export type Aim =
  | { kind: "own"; el: HTMLElement; pose: CardPose; opacity: number; faceImagesSettled: boolean }
  | { kind: "provisional"; el: HTMLElement | null; pose: CardPose }
  | { kind: "hold" };

/** A face is the `<img>` covering at least this fraction of its own node's
 *  layout box. Every surface's face covers at least 0.42 of it, while mana
 *  pips cover under 0.01. */
export const FACE_IMAGE_MIN_AREA_FRACTION = 0.25;

/** Measures `el` relative to `origin` (the overlay canvas rect). The centre
 *  comes from the viewport rect; size and angle come from layout size and the
 *  2D linear part of every transform from `el` up to the root, so a fanned,
 *  tapped or scaled card reports its own geometry rather than its bounding box.
 *  `opacity` is the product of every node's computed opacity from `el` up. */
export function measureSurface(
  el: HTMLElement,
  origin: DOMRectReadOnly,
): { pose: CardPose; opacity: number } {
  const rect = el.getBoundingClientRect();
  // Accumulated 2D linear map [[a c], [b d]]; an ancestor applies after its descendant.
  let a = 1;
  let b = 0;
  let c = 0;
  let d = 1;
  let opacity = 1;
  for (let node: Element | null = el; node; node = node.parentElement) {
    const style = getComputedStyle(node);
    // An empty value is the initial value (1), as an empty transform is `none`.
    opacity *= style.opacity === "" ? 1 : Number(style.opacity);
    const transform = style.transform;
    if (!transform || transform === "none") continue;
    const m = new DOMMatrixReadOnly(transform);
    [a, b, c, d] = [m.a * a + m.c * b, m.b * a + m.d * b, m.a * c + m.c * d, m.b * c + m.d * d];
  }
  return {
    pose: {
      x: rect.left + rect.width / 2 - origin.left,
      y: rect.top + rect.height / 2 - origin.top,
      w: el.offsetWidth * Math.hypot(a, b),
      h: el.offsetHeight * Math.hypot(c, d),
      angleDeg: (Math.atan2(b, a) * 180) / Math.PI,
    },
    opacity,
  };
}

export function measureCardPose(el: HTMLElement, origin: DOMRectReadOnly): CardPose {
  return measureSurface(el, origin).pose;
}

/** Whether `el` shows its face: it holds at least one face `<img>` (one whose
 *  layout box covers `FACE_IMAGE_MIN_AREA_FRACTION` of `el`'s) and every face
 *  `<img>` is `complete`. Layout boxes ignore transforms, so the image and its
 *  node compare in the same untransformed frame. `complete` is not painted: the
 *  first paint of a newly available image may trail it by about one decode. */
export function faceImagesSettled(el: HTMLElement): boolean {
  const minArea = FACE_IMAGE_MIN_AREA_FRACTION * el.offsetWidth * el.offsetHeight;
  const faces = [...el.querySelectorAll("img")].filter((img) => img.offsetWidth * img.offsetHeight >= minArea);
  return faces.length > 0 && faces.every((img) => img.complete);
}

/** The first match in document order that is laid out (non-zero width). A
 *  veiled surface is still laid out; a held or `display: none` card is not. */
export function firstRendered(selector: string): HTMLElement | null {
  for (const el of document.querySelectorAll<HTMLElement>(selector)) {
    if (el.offsetWidth > 0) return el;
  }
  return null;
}

function lastRendered(selector: string): HTMLElement | null {
  const matches = [...document.querySelectorAll<HTMLElement>(selector)];
  return matches.reverse().find((el) => el.offsetWidth > 0) ?? null;
}

/** The first selector, in priority order, with a laid-out match. */
function firstRenderedOf(selectors: readonly string[]): HTMLElement | null {
  for (const selector of selectors) {
    const el = firstRendered(selector);
    if (el) return el;
  }
  return null;
}

// Zone-scoped anchors (the phase 1 anchor contract). The generic
// `[data-object-id]` matches one object in several zones at once.
const stackEntrySelectors = (id: ObjectId) => [
  `[data-stack-entry="${id}"]`,
  `[data-stack-entry][data-grouped-ids~="${id}"]`,
];
const permanentSelectors = (id: ObjectId) => [
  `[data-permanent-card="${id}"]`,
  `[data-permanent-card][data-grouped-ids~="${id}"]`,
];
const castSourceSelectors = (id: ObjectId) => [
  `[data-hand-card][data-object-id="${id}"]`,
  `[data-zone-fan-card][data-object-id="${id}"]`,
  `[data-opponent-hand-card="${id}"]`,
  `[data-graveyard-pile][data-grouped-ids~="${id}"]`,
  `[data-library-pile] [data-grouped-ids~="${id}"]`,
];

/** Where a cast flight starts: the card's veil-aware surface in a cast-source
 *  zone. `null` (a pending cast already on the stack, a command-zone cast, the
 *  held mobile card) presents Classic. */
export function castSourceElement(id: ObjectId): HTMLElement | null {
  return firstRenderedOf(castSourceSelectors(id));
}

/** Where a resolve flight starts: the object's stack entry. */
export function stackSourceElement(id: ObjectId): HTMLElement | null {
  return firstRenderedOf(stackEntrySelectors(id));
}

/** The source surface for a flight on `route`. */
export function sourceElement(route: CardFlightRoute, id: ObjectId): HTMLElement | null {
  switch (route.kind) {
    case "cast":
      return castSourceElement(id);
    case "resolveToBattlefield":
    case "resolveToGraveyard":
      return stackSourceElement(id);
  }
}

/** The object's own surface in the route's destination zone — the only node a
 *  flight lands on. It exists only once the engine commit has moved the object. */
export function ownNode(route: CardFlightRoute, id: ObjectId): HTMLElement | null {
  switch (route.kind) {
    case "cast":
      return firstRenderedOf(stackEntrySelectors(id));
    case "resolveToBattlefield":
      return firstRenderedOf(permanentSelectors(id));
    case "resolveToGraveyard":
      return firstRendered(`[data-graveyard-pile="${route.ownerId}"][data-grouped-ids~="${id}"]`);
  }
}

/** A node that stands in for the destination before the own node exists. */
export function provisionalNode(route: CardFlightRoute): HTMLElement | null {
  switch (route.kind) {
    case "cast":
      return lastRendered("[data-stack-entry]");
    case "resolveToBattlefield":
      return null;
    case "resolveToGraveyard":
      return firstRendered(`[data-graveyard-pile="${route.ownerId}"]`);
  }
}

/** This frame's aim for object `id` on `route`: its own node, else a
 *  provisional node, else — for a cast — the last stack pose measured, else hold. */
export function resolveAim(
  route: CardFlightRoute,
  id: ObjectId,
  origin: DOMRectReadOnly,
  lastStackPose: CardPose | null,
): Aim {
  const own = ownNode(route, id);
  if (own) return { kind: "own", el: own, ...measureSurface(own, origin), faceImagesSettled: faceImagesSettled(own) };
  const provisional = provisionalNode(route);
  if (provisional) {
    return { kind: "provisional", el: provisional, pose: measureCardPose(provisional, origin) };
  }
  if (route.kind === "cast" && lastStackPose) {
    return { kind: "provisional", el: null, pose: lastStackPose };
  }
  return { kind: "hold" };
}
