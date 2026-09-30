import type { GameState, ManaColor, ObjectId, PlayerId, Zone } from "../../../adapter/types.ts";
import type { AnimationEvent } from "../../../animation/types.ts";
import {
  type AnimationImageSnapshot,
  visibleAnimationImageSnapshot,
} from "../ResolvedAnimationImage.tsx";

/** The zones a card flight can land in. Every per-destination table is keyed
 *  by this, so a new destination is a compile error until each has its entry. */
export type FlightDestination = Extract<Zone, "Stack" | "Battlefield" | "Graveyard" | "Hand">;

/** Where a card flight goes: from the card's surface in one zone to its own
 *  surface in another. `ownerId` locates per-player surfaces (hand, library,
 *  graveyard), which belong to the card's owner whoever controls it. */
export interface CardFlightRoute {
  from: Zone;
  to: FlightDestination;
  ownerId: PlayerId;
}

/** The zone changes a card flight presents, by origin. A move between zones
 *  that already have surfaces needs only its entry here. Casting is not a zone
 *  change event: `castOf` routes it. */
const FLIGHT_ZONE_CHANGES: { readonly [From in Zone]?: readonly FlightDestination[] } = {
  // CR 608.2n / CR 608.3: a resolving spell goes to the battlefield or its owner's graveyard.
  Stack: ["Battlefield", "Graveyard"],
  // CR 121.1: a drawn card moves from the library to the hand.
  Library: ["Hand"],
  // CR 305.1: a played land (or a card an effect puts onto the battlefield)
  // moves from the hand to the battlefield.
  Hand: ["Battlefield"],
};

export interface CardFlightSpec {
  kind: "flight";
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

/** The gap between consecutive flights of one zone change in a step, before pace. */
export const FLIGHT_STAGGER_MS = 90;

interface RoutedObject {
  objectId: ObjectId;
  route: CardFlightRoute;
}

/** CR 601.2a: casting moves the card from where it is to the stack. The event
 *  announcing the cast (or, for a cast that completes in one step, the cast
 *  itself) starts the flight from the zone the card is cast from. */
function castOf(objectId: ObjectId, pre: GameState | null, post: GameState | null): RoutedObject | null {
  const from = pre?.objects[objectId]?.zone;
  const owner = post?.objects[objectId]?.owner;
  return from !== undefined && owner !== undefined
    ? { objectId, route: { from, to: "Stack", ownerId: owner } }
    : null;
}

function zoneChangeOf(event: AnimationEvent, post: GameState | null): RoutedObject | null {
  if (event.type !== "ZoneChanged") return null;
  const { object_id: objectId, from } = event.data;
  const to = FLIGHT_ZONE_CHANGES[from]?.find((destination) => destination === event.data.to);
  const owner = post?.objects[objectId]?.owner;
  return to && owner !== undefined ? { objectId, route: { from, to, ownerId: owner } } : null;
}

/** CR 601.2a: whether a spell was announced in an earlier batch whose cast
 *  paused for a choice; it went to the stack at its announcement. */
export function castAnnounced(objectId: ObjectId, pre: GameState | null): boolean {
  return pre?.stack.some((entry) => entry.id === objectId) ?? false;
}

function routedObjectFor(event: AnimationEvent, pre: GameState | null, post: GameState | null): RoutedObject | null {
  switch (event.type) {
    case "SpellCast":
      return castAnnounced(event.data.object_id, pre) ? null : castOf(event.data.object_id, pre, post);
    case "StackPushed":
      return castOf(event.data.object_id, pre, post);
    default:
      return zoneChangeOf(event, post);
  }
}

const sameZoneChange = (a: AnimationEvent, b: AnimationEvent) =>
  a.type === "ZoneChanged" &&
  b.type === "ZoneChanged" &&
  a.data.from === b.data.from &&
  a.data.to === b.data.to;

/** When a flight leaves after its step starts. Flights of one zone change in a
 *  step (a multi-card draw) leave one after another: the nth leaves n gaps in,
 *  and the gaps shrink so the last still leaves within the step's first half. */
function delayFor(event: AnimationEvent, stepEvents: readonly AnimationEvent[], pace: number, owningStepMs: number) {
  const batch = stepEvents.filter((other) => sameZoneChange(event, other));
  const index = batch.indexOf(event);
  if (index <= 0) return 0;
  const gap = Math.min(FLIGHT_STAGGER_MS * pace, owningStepMs / 2 / (batch.length - 1));
  return index * gap;
}

/** The card flight that presents `event`, or `null` when the event has no
 *  flight (it then presents Classic). A zero or negative pace has no flight,
 *  matching the step timers' instant mode. */
export function cardFlightSpecFor(
  event: AnimationEvent,
  { pre, post, pace, owningStepMs, stepEvents }: CardFlightSpecContext,
): CardFlightSpec | null {
  if (pace <= 0) return null;
  const routed = routedObjectFor(event, pre, post);
  if (!routed) return null;
  const { objectId } = routed;
  const endObject = post?.objects[objectId];
  const endFace = visibleAnimationImageSnapshot(endObject);
  return {
    kind: "flight",
    ...routed,
    startFace: visibleAnimationImageSnapshot(pre?.objects[objectId]),
    endFace,
    endColors: endFace && endObject ? endObject.color : null,
    pace,
    owningStepMs,
    delayMs: delayFor(event, stepEvents, pace, owningStepMs),
  };
}
