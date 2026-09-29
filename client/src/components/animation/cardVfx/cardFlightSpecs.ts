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
  | { kind: "resolveToGraveyard"; ownerId: PlayerId };

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
}

export interface CardFlightSpecContext {
  /** The committed state the event starts from. */
  pre: GameState | null;
  /** The state the event produces. */
  post: GameState | null;
  pace: number;
  owningStepMs: number;
}

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
export function cardFlightSpecFor(
  event: AnimationEvent,
  { pre, post, pace, owningStepMs }: CardFlightSpecContext,
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
  };
}
