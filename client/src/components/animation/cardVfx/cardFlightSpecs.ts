import type { GameState, ManaColor, ObjectId, PlayerId, Zone } from "../../../adapter/types.ts";
import type { AnimationEvent } from "../../../animation/types.ts";
import {
  type AnimationImageSnapshot,
  visibleAnimationImageSnapshot,
} from "../ResolvedAnimationImage.tsx";

/** The zones a card flight can land in. Every per-destination table is keyed
 *  by this, so a new destination is a compile error until each has its entry. */
export type FlightDestination = Extract<Zone, "Stack" | "Battlefield" | "Graveyard" | "Hand" | "Library" | "Exile">;

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
 *  change event: `castOf` routes it. A permanent leaving the battlefield for a
 *  graveyard is not either: its destruction breaks it where it lies, its
 *  sacrifice (`sacrificeOf`) flies, and its exile dissolves. */
const FLIGHT_ZONE_CHANGES: { readonly [From in Zone]?: readonly FlightDestination[] } = {
  // CR 608.2n / CR 608.3: a resolving spell goes to the battlefield or its
  // owner's graveyard; a countered or bounced one to its hand, library or exile.
  Stack: ["Battlefield", "Graveyard", "Hand", "Library", "Exile"],
  // CR 121.1: a drawn card moves from the library to the hand; CR 701.17a: a
  // milled one to the graveyard. Effects also exile or put cards onto the battlefield.
  Library: ["Hand", "Graveyard", "Exile", "Battlefield"],
  // CR 305.1: a played land (or a card an effect puts onto the battlefield)
  // moves from the hand to the battlefield; CR 701.9a: a discarded card to the graveyard.
  Hand: ["Battlefield", "Graveyard", "Library", "Exile"],
  Graveyard: ["Hand", "Battlefield", "Library", "Exile"],
  Exile: ["Hand", "Battlefield", "Graveyard", "Library"],
  Battlefield: ["Hand", "Library"],
};

/** Whether a card flight presents a move from `from` to `to`. */
export function flightPresents(from: Zone, to: Zone): boolean {
  return FLIGHT_ZONE_CHANGES[from]?.some((destination) => destination === to) ?? false;
}

/** The most flights one batch sends (a mass mill or graveyard exile); the
 *  rest present Classic. */
export const FLIGHT_BATCH_MAX = 12;

export interface CardFlightSpec {
  kind: "flight";
  objectId: ObjectId;
  /** The object whose surface the flight leaves from: the card itself, or the
   *  source a token comes out of. */
  sourceId: ObjectId;
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
  /** The engine commit epoch the owning step was queued under
   *  (`QueuedStep.commitEpoch`): a later epoch means its state has committed. */
  commitEpoch: number;
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
  /** The engine commit epoch the owning step was queued under
   *  (`QueuedStep.commitEpoch`): a later epoch means its state has committed. */
  commitEpoch: number;
  /** Every event in the owning step, in order. */
  stepEvents: readonly AnimationEvent[];
}

/** The gap between consecutive flights of one zone change in a step, before pace. */
export const FLIGHT_STAGGER_MS = 90;

interface RoutedObject {
  objectId: ObjectId;
  sourceId: ObjectId;
  route: CardFlightRoute;
}

/** CR 601.2a: casting moves the card from where it is to the stack. The event
 *  announcing the cast (or, for a cast that completes in one step, the cast
 *  itself) starts the flight from the zone the card is cast from. */
function castOf(objectId: ObjectId, pre: GameState | null, post: GameState | null): RoutedObject | null {
  const from = pre?.objects[objectId]?.zone;
  const owner = post?.objects[objectId]?.owner;
  return from !== undefined && owner !== undefined
    ? { objectId, sourceId: objectId, route: { from, to: "Stack", ownerId: owner } }
    : null;
}

function zoneChangeOf(event: AnimationEvent, post: GameState | null): RoutedObject | null {
  if (event.type !== "ZoneChanged" || event.data.from === null) return null;
  const { object_id: objectId, from } = event.data;
  const to = FLIGHT_ZONE_CHANGES[from]?.find((destination) => destination === event.data.to);
  const owner = post?.objects[objectId]?.owner;
  return to && owner !== undefined ? { objectId, sourceId: objectId, route: { from, to, ownerId: owner } } : null;
}

/** CR 701.21a: a sacrificed permanent moves from the battlefield to its
 *  owner's graveyard. A replacement (CR 614.1a) that sends it elsewhere leaves
 *  its zone change to present the move, and a token that ceased to exist in
 *  the graveyard (CR 111.7) has no card to fly. */
function sacrificeOf(objectId: ObjectId, post: GameState | null): RoutedObject | null {
  const object = post?.objects[objectId];
  return object?.zone === "Graveyard"
    ? { objectId, sourceId: objectId, route: { from: "Battlefield", to: "Graveyard", ownerId: object.owner } }
    : null;
}

/** CR 111.1: an effect puts a token onto the battlefield; it comes out of the
 *  source that created it, when that source is a spell on the stack or a
 *  permanent. */
function tokenOf(objectId: ObjectId, sourceId: ObjectId, pre: GameState | null, post: GameState | null): RoutedObject | null {
  const from = pre?.objects[sourceId]?.zone;
  const owner = post?.objects[objectId]?.owner;
  return (from === "Stack" || from === "Battlefield") && owner !== undefined
    ? { objectId, sourceId, route: { from, to: "Battlefield", ownerId: owner } }
    : null;
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
    case "PermanentSacrificed":
      return sacrificeOf(event.data.object_id, post);
    case "TokenCreated":
      return tokenOf(event.data.object_id, event.data.source_id, pre, post);
    default:
      return zoneChangeOf(event, post);
  }
}

/** The flights of one step that leave one after another: one zone change's
 *  cards, one step's sacrifices, one step's tokens. */
function batchKey(event: AnimationEvent): string | null {
  switch (event.type) {
    case "ZoneChanged":
      return `${event.data.from}>${event.data.to}`;
    case "PermanentSacrificed":
    case "TokenCreated":
      return event.type;
    default:
      return null;
  }
}

/** Where `event` falls in its step's batch of flights (see `batchKey`). */
function batchIndex(event: AnimationEvent, stepEvents: readonly AnimationEvent[]): { index: number; size: number } {
  const key = batchKey(event);
  const batch = key === null ? [event] : stepEvents.filter((other) => batchKey(other) === key);
  return { index: Math.max(batch.indexOf(event), 0), size: batch.length };
}

/** When a flight leaves after its step starts. Flights of one batch in a step
 *  (a multi-card draw) leave one after another: the nth leaves n gaps in, and
 *  the gaps shrink so the last still leaves within the step's first half. */
function delayFor({ index, size }: { index: number; size: number }, pace: number, owningStepMs: number) {
  if (index === 0) return 0;
  const flights = Math.min(size, FLIGHT_BATCH_MAX);
  const gap = Math.min(FLIGHT_STAGGER_MS * pace, owningStepMs / 2 / (flights - 1));
  return index * gap;
}

/** The card flight that presents `event`, or `null` when the event has no
 *  flight (it then presents Classic). A zero or negative pace has no flight,
 *  matching the step timers' instant mode. */
export function cardFlightSpecFor(
  event: AnimationEvent,
  { pre, post, pace, owningStepMs, commitEpoch, stepEvents }: CardFlightSpecContext,
): CardFlightSpec | null {
  if (pace <= 0) return null;
  const routed = routedObjectFor(event, pre, post);
  if (!routed) return null;
  const batch = batchIndex(event, stepEvents);
  if (batch.index >= FLIGHT_BATCH_MAX) return null;
  const { objectId } = routed;
  const endObject = post?.objects[objectId];
  const endFace = visibleAnimationImageSnapshot(endObject);
  // A token has no face before it exists; it comes out of its source already showing it.
  const startFace = routed.sourceId === objectId ? visibleAnimationImageSnapshot(pre?.objects[objectId]) : endFace;
  return {
    kind: "flight",
    ...routed,
    startFace,
    endFace,
    endColors: endFace && endObject ? endObject.color : null,
    pace,
    owningStepMs,
    commitEpoch,
    delayMs: delayFor(batch, pace, owningStepMs),
  };
}
