import type { ObjectId, PlayerId, Zone } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { resolvePileSeat } from "../../../viewmodel/gameStateView.ts";
import type { CardFlightRoute, FlightDestination } from "./cardFlightSpecs.ts";

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

/** The 2D linear part of a node's own transforms: its `rotate` and `scale`
 *  properties, applied after its `transform` (CSS Transforms 2), or `null`
 *  when it has none. A 3D `rotate` (an axis and an angle) is left out. */
function nodeLinear(style: CSSStyleDeclaration): DOMMatrixReadOnly | null {
  const { transform } = style;
  const rotate = style.getPropertyValue("rotate");
  const scale = style.getPropertyValue("scale");
  const has = (value: string) => value !== "" && value !== "none";
  if (!has(transform) && !has(rotate) && !has(scale)) return null;
  let m = new DOMMatrix();
  if (has(rotate) && /^-?[\d.]+deg$/.test(rotate)) m = m.rotate(parseFloat(rotate));
  if (has(scale)) {
    const [sx, sy = sx] = scale.split(" ").map(Number);
    m = m.scale(sx, sy);
  }
  return has(transform) ? m.multiply(new DOMMatrixReadOnly(transform)) : m;
}

/** Measures `el` relative to `origin` (the overlay canvas rect). The centre
 *  comes from the viewport rect; size and angle come from layout size and the
 *  2D linear part of every transform from `el` up to the root, so a fanned,
 *  tapped, scaled or slamming card reports its own geometry rather than its
 *  bounding box.
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
    const m = nodeLinear(style);
    if (!m) continue;
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
  const faces = faceImages(el);
  return faces.length > 0 && faces.every((img) => img.complete);
}

/** Card surfaces that can nest inside another: a permanent's attachments and
 *  the cards it holds in exile render inside its own node. */
const NESTED_SURFACES = "[data-permanent-card], [data-exile-ghost]";

/** The face `<img>`s in `el`: those whose layout box covers at least
 *  `FACE_IMAGE_MIN_AREA_FRACTION` of `el`'s, in document order, excluding the
 *  faces of card surfaces nested inside it. */
export function faceImages(el: HTMLElement): HTMLImageElement[] {
  const minArea = FACE_IMAGE_MIN_AREA_FRACTION * el.offsetWidth * el.offsetHeight;
  return [...el.querySelectorAll("img")].filter((img) => {
    const surface = img.closest(NESTED_SURFACES);
    const nested = surface !== null && surface !== el && el.contains(surface);
    return !nested && img.offsetWidth * img.offsetHeight >= minArea;
  });
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

/** The seat whose pile node holds `ownerId`'s `zone` cards: the shared pile's holder when the format shares it. */
const pileSeat = (zone: "library" | "graveyard", ownerId: PlayerId) =>
  resolvePileSeat(useGameStore.getState().gameState, zone, ownerId);

/** Permanent `id`'s card, else the collapsed group standing in for it. */
const permanentSelectors = (id: ObjectId) => [
  `[data-permanent-card="${id}"]`,
  `[data-permanent-card][data-grouped-ids~="${id}"]`,
];

// Zone-scoped anchors (the phase 1 anchor contract). The generic
// `[data-object-id]` matches one object in several zones at once.
/** The selectors, in priority order, for object `id`'s surface in each zone.
 *  A zone with none (the command zone) has no surface to fly from or to. A
 *  hidden library card shows as its pile (the holder's, in a shared-zone format). */
const ZONE_SURFACES: Record<Zone, (id: ObjectId, ownerId: PlayerId) => readonly string[]> = {
  Hand: (id) => [`[data-hand-card][data-object-id="${id}"]`, `[data-opponent-hand-card="${id}"]`],
  Library: (id, ownerId) => [
    `[data-library-pile] [data-grouped-ids~="${id}"]`,
    `[data-library-pile="${pileSeat("library", ownerId)}"]`,
  ],
  Graveyard: (id, ownerId) => [
    `[data-zone-fan-card="Graveyard"][data-object-id="${id}"]`,
    `[data-graveyard-pile="${pileSeat("graveyard", ownerId)}"][data-grouped-ids~="${id}"]`,
  ],
  // A face-down exiled card is only in its owner's pile's count.
  Exile: (id, ownerId) => [
    `[data-exile-ghost="${id}"]`,
    `[data-zone-fan-card="Exile"][data-object-id="${id}"]`,
    `[data-exile-pile="${ownerId}"]`,
  ],
  Stack: (id) => [`[data-stack-entry="${id}"]`, `[data-stack-entry][data-grouped-ids~="${id}"]`],
  Battlefield: permanentSelectors,
  Command: () => [],
};

/** Permanent `id`'s own laid-out surface, never a collapsed group's
 *  representative standing in for it: a board effect veils the node it
 *  replaces, and a representative's veil belongs to another object. */
export function ownPermanentSurface(id: ObjectId): HTMLElement | null {
  return firstRendered(`[data-permanent-card="${id}"]`);
}

/** Where an effect that veils nothing plays over permanent `id`: its own
 *  laid-out surface, else its collapsed group's representative. */
export function permanentSurface(id: ObjectId): HTMLElement | null {
  return firstRenderedOf(permanentSelectors(id));
}

/** Player `id`'s HUD, where damage to them lands. */
export function playerHudSurface(id: PlayerId): HTMLElement | null {
  return firstRendered(`[data-player-hud="${id}"]`);
}

/** Object `id`'s ghost under the permanent holding it in exile, veil-aware. */
export function exileGhostNode(id: ObjectId): HTMLElement | null {
  return firstRendered(`[data-exile-ghost="${id}"]`);
}

/** Nodes that stand in for a destination before the object's own node exists. */
const PROVISIONAL_SURFACES: Record<FlightDestination, (ownerId: PlayerId) => HTMLElement | null> = {
  Stack: () => lastRendered("[data-stack-entry]"),
  Graveyard: (ownerId) => firstRendered(`[data-graveyard-pile="${pileSeat("graveyard", ownerId)}"]`),
  Battlefield: () => null,
  Hand: () => null,
  // Their own surfaces fall back to the owner's pile, which stands in for itself.
  Library: () => null,
  Exile: () => null,
};

/** Object `id`'s laid-out surface in `zone`: its veil-aware card, or the
 *  owner's pile standing in for it. */
export function zoneSurface(zone: Zone, id: ObjectId, ownerId: PlayerId): HTMLElement | null {
  return firstRenderedOf(ZONE_SURFACES[zone](id, ownerId));
}

/** The surface a flight on `route` starts from: its source object's (the card
 *  itself, or a token's source) in the route's origin zone. `null` (a pending
 *  cast already on the stack, a command-zone cast, the held mobile card)
 *  presents Classic. */
export function sourceElement(route: CardFlightRoute, sourceId: ObjectId): HTMLElement | null {
  return zoneSurface(route.from, sourceId, route.ownerId);
}

/** The object's own surface in the route's destination zone — the only node a
 *  flight lands on. It exists only once the engine commit has moved the object. */
export function ownNode(route: CardFlightRoute, id: ObjectId): HTMLElement | null {
  return zoneSurface(route.to, id, route.ownerId);
}

/** A node that stands in for the destination before the own node exists. */
export function provisionalNode(route: CardFlightRoute): HTMLElement | null {
  return PROVISIONAL_SURFACES[route.to](route.ownerId);
}

/** This frame's aim for object `id` on `route`: its own node, else a
 *  provisional node, else — onto the stack — the last stack pose measured, else hold. */
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
  if (route.to === "Stack" && lastStackPose) {
    return { kind: "provisional", el: null, pose: lastStackPose };
  }
  return { kind: "hold" };
}
