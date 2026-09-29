import type { GameState, ManaColor, ObjectId, PlayerId } from "../../../adapter/types.ts";
import type { AnimationEvent } from "../../../animation/types.ts";
import {
  type AnimationImageSnapshot,
  visibleAnimationImageSnapshot,
} from "../ResolvedAnimationImage.tsx";

/** Where a card flight goes. Every per-route table is keyed by `kind`, so a
 *  new route is a compile error until each table has its entry. */
export type CardFlightRoute =
  | { kind: "cast" }
  | { kind: "resolveToBattlefield" }
  | { kind: "resolveToGraveyard"; ownerId: PlayerId }
  | { kind: "draw"; ownerId: PlayerId };

export interface CardFlightSpec {
  objectId: ObjectId;
  route: CardFlightRoute;
  /** The face the viewer may see before the event, from the engine's
   *  per-viewer visibility; `null` shows the card back. */
  startFace: AnimationImageSnapshot | null;
  /** The face the viewer may see after the event; `null` shows the card back. */
  endFace: AnimationImageSnapshot | null;
  /** The engine's colours for the card after the event, or `null` when the
   *  viewer may not see its face. */
  endColors: readonly ManaColor[] | null;
  /** The animation-speed multiplier every flight duration is scaled by. */
  pace: number;
  /** The owning step's scaled duration, which bounds face readiness. */
  owningStepMs: number;
  /** How long after the step starts this flight leaves: draws in one step
   *  leave one after another, all inside the step's first half. */
  delayMs: number;
}

export interface CardFlightSpecContext {
  /** The committed state the event starts from. */
  pre: GameState | null;
  /** The state the event produces. */
  post: GameState | null;
  pace: number;
  owningStepMs: number;
  /** Every event in the owning step, in order. */
  stepEvents: readonly AnimationEvent[];
}

/** The gap between consecutive draws in one step, before pace. */
export const DRAW_STAGGER_MS = 90;

interface RoutedObject {
  objectId: ObjectId;
  route: CardFlightRoute;
}

function routedObjectFor(event: AnimationEvent, post: GameState | null): RoutedObject | null {
  switch (event.type) {
    case "SpellCast":
      return { objectId: event.data.object_id, route: { kind: "cast" } };
    case "ZoneChanged": {
      const { object_id: objectId, from, to } = event.data;
      if (from === "Library" && to === "Hand") {
        // A card is drawn into its owner's hand.
        const object = post?.objects[objectId];
        return object ? { objectId, route: { kind: "draw", ownerId: object.owner } } : null;
      }
      if (from !== "Stack") return null;
      if (to === "Battlefield") return { objectId, route: { kind: "resolveToBattlefield" } };
      if (to !== "Graveyard") return null;
      // A spell goes to its owner's graveyard, whoever controlled it.
      const object = post?.objects[objectId];
      return object
        ? { objectId, route: { kind: "resolveToGraveyard", ownerId: object.owner } }
        : null;
    }
    default:
      return null;
  }
}

/** The card flight that presents `event`, or `null` when the event has no
 *  flight (it then presents Classic). A zero or negative pace has no flight,
 *  matching the step timers' instant mode. */
const isDraw = (event: AnimationEvent) =>
  event.type === "ZoneChanged" && event.data.from === "Library" && event.data.to === "Hand";

/** When a flight leaves after its step starts. Only draws stagger: the nth
 *  draw of the step leaves n gaps in, and the gaps shrink so the last draw
 *  still leaves within the step's first half. */
function delayFor(event: AnimationEvent, stepEvents: readonly AnimationEvent[], pace: number, owningStepMs: number) {
  if (!isDraw(event)) return 0;
  const draws = stepEvents.filter(isDraw);
  const index = draws.indexOf(event);
  if (index <= 0) return 0;
  const gap = Math.min(DRAW_STAGGER_MS * pace, owningStepMs / 2 / (draws.length - 1));
  return index * gap;
}

export function cardFlightSpecFor(
  event: AnimationEvent,
  { pre, post, pace, owningStepMs, stepEvents }: CardFlightSpecContext,
): CardFlightSpec | null {
  if (pace <= 0) return null;
  const routed = routedObjectFor(event, post);
  if (!routed) return null;
  const { objectId } = routed;
  const endObject = post?.objects[objectId];
  const endFace = visibleAnimationImageSnapshot(endObject);
  return {
    ...routed,
    startFace: visibleAnimationImageSnapshot(pre?.objects[objectId]),
    endFace,
    endColors: endFace && endObject ? endObject.color : null,
    pace,
    owningStepMs,
    delayMs: delayFor(event, stepEvents, pace, owningStepMs),
  };
}
